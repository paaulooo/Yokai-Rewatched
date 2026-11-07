//! Uma das 24 fontes de voz do DSP (`audio_core/hle/source.cpp` do Citra/Azahar): lê a configuração da região
//! (dirty flags), mantém a fila de buffers, decodifica, reamostra, filtra e mistura nos 3 mixers intermediários.

use super::codec::{self, AdpcmState, Buffer, Filters, InterpState, Stereo};
use super::layout::{rd16, rd32, rd32d, rd8, rdf32, src, status, wr16, wr32, wr32d, SAMPLES_PER_FRAME};

pub type Frame = [Stereo; SAMPLES_PER_FRAME];
pub type QuadFrame = [[i32; 4]; SAMPLES_PER_FRAME];

/// Acesso às amostras na memória do jogo, por endereço físico (o DSP usa DMA físico).
pub trait SampleMemory {
    /// `len` bytes a partir do endereço físico `pa`, se toda a faixa estiver mapeada.
    fn read(&self, pa: u32, len: usize) -> Option<Vec<u8>>;
}

pub const FMT_PCM8: u16 = 0;
pub const FMT_PCM16: u16 = 1;
pub const FMT_ADPCM: u16 = 2;
const STEREO: u16 = 2;

#[derive(Clone, Copy, Debug)]
struct QueuedBuffer {
    pa: u32,
    length: u32,
    adpcm_yn: [u16; 2],
    adpcm_dirty: bool,
    is_looping: bool,
    buffer_id: u16,
    mono_or_stereo: u16,
    format: u16,
    from_queue: bool,
    play_position: u32,
    has_played: bool,
}

#[derive(Clone, Debug)]
pub struct State {
    pub enabled: bool,
    sync_count: u16,
    gain: [[f32; 4]; 3],
    gain_ramp_start: [[f32; 4]; 3],
    gain_ramp_active: [bool; 3],
    /// fila de buffers; sai primeiro o de menor `buffer_id` (priority_queue do Citra)
    queue: Vec<QueuedBuffer>,
    mono_or_stereo: u16,
    pub format: u16,
    current_sample: u32,
    current_pa: u32,
    current_length: u32,
    current_mono_or_stereo: u16,
    current_format: u16,
    current_is_looping: bool,
    current: Buffer,
    buffer_update: bool,
    last_buffer_id: u16,
    current_buffer_id: u16,
    adpcm_coeffs: [i16; 16],
    adpcm: AdpcmState,
    rate: f32,
    /// 0 polifásica (o Citra usa a linear no lugar), 1 linear, 2 nenhuma
    interp_mode: u8,
    interp: InterpState,
    filters: Filters,
}

impl Default for State {
    fn default() -> Self {
        State {
            enabled: false,
            sync_count: 0,
            gain: [[0.0; 4]; 3],
            gain_ramp_start: [[0.0; 4]; 3],
            gain_ramp_active: [false; 3],
            queue: Vec::new(),
            mono_or_stereo: 1,
            format: FMT_ADPCM,
            current_sample: 0,
            current_pa: 0,
            current_length: 0,
            current_mono_or_stereo: 1,
            current_format: FMT_ADPCM,
            current_is_looping: false,
            current: Buffer::new(),
            buffer_update: false,
            last_buffer_id: 0,
            current_buffer_id: 0,
            adpcm_coeffs: [0; 16],
            adpcm: AdpcmState::default(),
            rate: 1.0,
            interp_mode: 0,
            interp: InterpState::default(),
            filters: Filters::default(),
        }
    }
}

#[derive(Clone)]
pub struct Source {
    pub id: usize,
    pub state: State,
    frame: Frame,
}

/// Status de uma fonte, como vai para `SourceStatus::Status`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Status {
    pub is_enabled: bool,
    pub buffer_id_dirty: bool,
    pub sync_count: u16,
    pub buffer_position: u32,
    pub current_buffer_id: u16,
    pub last_buffer_id: u16,
}

impl Status {
    pub fn write(&self, b: &mut [u8]) {
        b[status::IS_ENABLED] = self.is_enabled as u8;
        b[status::BUFFER_ID_DIRTY] = self.buffer_id_dirty as u8;
        wr16(b, status::SYNC_COUNT, self.sync_count);
        wr32d(b, status::BUFFER_POSITION, self.buffer_position);
        wr16(b, status::CURRENT_BUFFER_ID, self.current_buffer_id);
        wr16(b, status::LAST_BUFFER_ID, self.last_buffer_id);
    }
}

/// `YW_DSP_TRACE`: registra cada mudança de configuração das fontes (depuração).
pub fn trace() -> bool {
    static T: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *T.get_or_init(|| std::env::var_os("YW_DSP_TRACE").is_some())
}

fn channels(mono_or_stereo: u16) -> usize {
    if mono_or_stereo == STEREO {
        2
    } else {
        1
    }
}

impl Source {
    pub fn new(id: usize) -> Self {
        Source { id, state: State::default(), frame: [[0; 2]; SAMPLES_PER_FRAME] }
    }

    pub fn reset(&mut self) {
        self.frame = [[0; 2]; SAMPLES_PER_FRAME];
        self.state = State::default();
    }

    /// Um quadro de áudio. `cfg` é a configuração da fonte (192 bytes) na região lida: as dirty flags são
    /// zeradas nela, como o DSP faz. Devolve o status para a região de escrita.
    pub fn tick(&mut self, cfg: &mut [u8], coeffs: &[i16; 16], mem: &dyn SampleMemory) -> Status {
        self.parse_config(cfg, coeffs, mem);
        if self.state.enabled {
            self.generate_frame(mem);
        }
        self.current_status()
    }

    /// Soma a saída desta fonte no mixer intermediário `mix` (rampa de ganho linear ao longo do quadro quando o
    /// ganho mudou). Estéreo → quadrifônico: canais 0/2 = esquerdo, 1/3 = direito.
    pub fn mix_into(&mut self, dest: &mut QuadFrame, mix: usize) {
        let st = &mut self.state;
        let gains = st.gain[mix];
        if !st.enabled {
            st.gain_ramp_start[mix] = gains;
            st.gain_ramp_active[mix] = false;
            return;
        }
        let ramp = st.gain_ramp_active[mix];
        let start = st.gain_ramp_start[mix];
        let k = 1.0f32 / (SAMPLES_PER_FRAME - 1) as f32;
        for (i, d) in dest.iter_mut().enumerate() {
            let p = i as f32 * k;
            let g = |c: usize| if ramp { start[c] + (gains[c] - start[c]) * p } else { gains[c] };
            let s = self.frame[i];
            d[0] = d[0].wrapping_add((g(0) * s[0] as f32) as i32);
            d[1] = d[1].wrapping_add((g(1) * s[1] as f32) as i32);
            d[2] = d[2].wrapping_add((g(2) * s[0] as f32) as i32);
            d[3] = d[3].wrapping_add((g(3) * s[1] as f32) as i32);
        }
        if ramp {
            st.gain_ramp_start[mix] = gains;
            st.gain_ramp_active[mix] = false;
        }
    }

    fn parse_config(&mut self, cfg: &mut [u8], coeffs: &[i16; 16], mem: &dyn SampleMemory) {
        let mut dirty = rd32(cfg, src::DIRTY);
        if dirty == 0 {
            return;
        }
        if trace() {
            eprintln!(
                "[dsp-trace] fonte {} dirty {dirty:#010x} enable {} fmt {} ch {} taxa {} emb pa {:#x} len {} id {} pos {} flags2 {:#x} · fila {:#06b} ids {:?} · sync {}",
                self.id,
                rd8(cfg, src::ENABLE),
                (rd16(cfg, src::FLAGS1) >> 2) & 3,
                rd16(cfg, src::FLAGS1) & 3,
                rdf32(cfg, src::RATE),
                rd32d(cfg, src::PHYS_ADDR),
                rd32d(cfg, src::LENGTH),
                rd16(cfg, src::BUFFER_ID),
                rd32d(cfg, src::PLAY_POSITION),
                rd16(cfg, src::FLAGS2),
                rd16(cfg, src::BUFFERS_DIRTY),
                (0..4)
                    .map(|i| {
                        let b = src::BUFFERS + i * src::BUFFER_SIZE;
                        (rd16(cfg, b + src::buf::BUFFER_ID), rd32d(cfg, b + src::buf::PHYS_ADDR), rd32d(cfg, b + src::buf::LENGTH), rd8(cfg, b + src::buf::IS_LOOPING))
                    })
                    .collect::<Vec<_>>(),
                rd16(cfg, src::SYNC_COUNT),
            );
        }
        let st = &mut self.state;
        fn take_bit(d: &mut u32, bit: u32) -> bool {
            let on = *d & bit != 0;
            *d &= !bit;
            on
        }
        macro_rules! take {
            ($b:expr) => {
                take_bit(&mut dirty, $b)
            };
        }
        if take!(src::RESET) {
            self.frame = [[0; 2]; SAMPLES_PER_FRAME];
            *st = State::default();
        }
        if take!(src::PARTIAL_RESET) {
            st.queue.clear();
        }
        if take!(src::ENABLE_DIRTY) {
            st.enabled = rd8(cfg, src::ENABLE) != 0;
        }
        if take!(src::SYNC_COUNT_DIRTY) {
            st.sync_count = rd16(cfg, src::SYNC_COUNT);
        }
        if take!(src::RATE_DIRTY) {
            st.rate = rdf32(cfg, src::RATE);
            if !(st.rate > 0.0) {
                eprintln!("[dsp] fonte {}: taxa inválida {} (usando 1,0)", self.id, st.rate);
                st.rate = 1.0;
            }
        }
        if take!(src::ADPCM_COEFFS_DIRTY) {
            st.adpcm_coeffs = *coeffs;
        }
        for m in 0..3 {
            if take!(src::GAIN0_DIRTY << m) {
                st.gain_ramp_start[m] = st.gain[m];
                st.gain_ramp_active[m] = true;
                for c in 0..4 {
                    st.gain[m][c] = rdf32(cfg, src::GAIN + 16 * m + 4 * c);
                }
            }
        }
        if take!(src::FILTERS_ENABLED_DIRTY) {
            let f = rd16(cfg, src::FILTERS_ENABLED);
            st.filters.enable(f & 1 != 0, f & 2 != 0);
        }
        if take!(src::SIMPLE_FILTER_DIRTY) {
            st.filters.configure_simple(rd16(cfg, src::SIMPLE_B0) as i16, rd16(cfg, src::SIMPLE_A1) as i16);
        }
        if take!(src::BIQUAD_FILTER_DIRTY) {
            let r = |o| rd16(cfg, o) as i16;
            st.filters.configure_biquad(
                r(src::BIQUAD_A2),
                r(src::BIQUAD_A1),
                r(src::BIQUAD_B2),
                r(src::BIQUAD_B1),
                r(src::BIQUAD_B0),
            );
        }
        if take!(src::INTERP_DIRTY) {
            st.interp_mode = rd8(cfg, src::INTERP);
        }
        let embedded = dirty & src::EMBEDDED_DIRTY != 0;
        let flags1 = rd16(cfg, src::FLAGS1);
        if take!(src::FORMAT_DIRTY) || embedded {
            st.format = (flags1 >> 2) & 3;
        }
        if take!(src::MONO_STEREO_DIRTY) || embedded {
            st.mono_or_stereo = flags1 & 3;
        }
        // play_position vale só para o buffer embutido; sem a flag, começa do 0
        let play_position = if take!(src::PLAY_POSITION_DIRTY) { rd32d(cfg, src::PLAY_POSITION) } else { 0 };

        if take!(src::PARTIAL_EMBEDDED_DIRTY) {
            // Buffer embutido estendido enquanto toca: relê tudo com o novo tamanho (endereço travado) e
            // re-consome até a amostra atual. O Citra só implementa PCM16 aqui.
            let length = rd32d(cfg, src::LENGTH);
            let ch = channels(st.mono_or_stereo);
            match st.format {
                FMT_PCM16 => {
                    if let Some(d) = mem.read(st.current_pa & !3, codec::bytes_needed(FMT_PCM16, ch, length as usize)) {
                        st.current = codec::decode_pcm16(ch, &d, length as usize);
                        st.current_length = length;
                        st.current_mono_or_stereo = st.mono_or_stereo;
                        st.current_format = st.format;
                        st.current_is_looping = rd16(cfg, src::FLAGS2) & 2 != 0;
                        if st.current.len() < st.current_sample as usize {
                            st.current_sample = 0;
                        } else {
                            st.current.drain(..st.current_sample as usize);
                        }
                    }
                }
                f => eprintln!("[dsp] fonte {}: atualização parcial do buffer embutido no formato {f} (não tratada, como no Citra)", self.id),
            }
        }

        if take!(src::EMBEDDED_DIRTY) {
            let length = rd32d(cfg, src::LENGTH);
            if (length as i32) < 0 {
                eprintln!("[dsp] fonte {}: buffer embutido com tamanho inválido {length:#x} (ignorado)", self.id);
            } else {
                let flags2 = rd16(cfg, src::FLAGS2);
                st.queue.push(QueuedBuffer {
                    pa: rd32d(cfg, src::PHYS_ADDR),
                    length,
                    adpcm_yn: [rd16(cfg, src::ADPCM_YN), rd16(cfg, src::ADPCM_YN + 2)],
                    adpcm_dirty: flags2 & 1 != 0,
                    is_looping: flags2 & 2 != 0,
                    buffer_id: rd16(cfg, src::BUFFER_ID),
                    mono_or_stereo: st.mono_or_stereo,
                    format: st.format,
                    from_queue: false,
                    play_position,
                    has_played: false,
                });
            }
        }

        if dirty & src::LOOP_RELATED_DIRTY != 0 && rd32d(cfg, src::LOOP_RELATED) != 0 {
            dirty &= !src::LOOP_RELATED_DIRTY;
            eprintln!("[dsp] fonte {}: loop complexo loop_related={:#x} (não tratado, como no Citra)", self.id, rd32d(cfg, src::LOOP_RELATED));
        }

        if dirty & src::BUFFER_QUEUE_DIRTY != 0 {
            let mask = rd16(cfg, src::BUFFERS_DIRTY);
            for i in 0..4 {
                if mask & (1 << i) == 0 {
                    continue;
                }
                let b = src::BUFFERS + i * src::BUFFER_SIZE;
                let length = rd32d(cfg, b + src::buf::LENGTH);
                if (length as i32) < 0 {
                    eprintln!("[dsp] fonte {}: buffer da fila com tamanho inválido {length:#x} (ignorado)", self.id);
                    continue;
                }
                st.queue.push(QueuedBuffer {
                    pa: rd32d(cfg, b + src::buf::PHYS_ADDR),
                    length,
                    adpcm_yn: [rd16(cfg, b + src::buf::ADPCM_YN), rd16(cfg, b + src::buf::ADPCM_YN + 2)],
                    adpcm_dirty: rd8(cfg, b + src::buf::ADPCM_DIRTY) != 0,
                    is_looping: rd8(cfg, b + src::buf::IS_LOOPING) != 0,
                    buffer_id: rd16(cfg, b + src::buf::BUFFER_ID),
                    mono_or_stereo: st.mono_or_stereo,
                    format: st.format,
                    from_queue: true,
                    play_position: 0,
                    has_played: false,
                });
            }
            wr16(cfg, src::BUFFERS_DIRTY, 0);
        }
        // o DSP zera todas as flags (inclusive as que não trata)
        wr32(cfg, src::DIRTY, 0);
    }

    fn generate_frame(&mut self, mem: &dyn SampleMemory) {
        self.frame = [[0; 2]; SAMPLES_PER_FRAME];
        let st = &mut self.state;
        // Buffer PCM16 em loop pode ser reescrito pelo jogo enquanto toca: relê da RAM só a parte não consumida.
        if !st.current.is_empty()
            && st.current_is_looping
            && st.current_format == FMT_PCM16
            && st.current_pa != 0
            && st.current_sample < st.current_length
        {
            let ch = channels(st.current_mono_or_stereo);
            let pa = (st.current_pa & !3) + st.current_sample * ch as u32 * 2;
            let rest = (st.current_length - st.current_sample) as usize;
            if let Some(d) = mem.read(pa, rest * ch * 2) {
                st.current = codec::decode_pcm16(ch, &d, rest);
            }
        }

        if st.current.is_empty() {
            if self.dequeue(mem) {
                return;
            }
            let st = &mut self.state;
            st.enabled = false;
            st.buffer_update = true;
            st.last_buffer_id = st.current_buffer_id;
            st.current_buffer_id = 0;
            return;
        }

        let mut pos = 0usize;
        while pos < SAMPLES_PER_FRAME {
            if self.state.current.is_empty() && !self.dequeue(mem) {
                break;
            }
            let st = &mut self.state;
            let before = st.current.len();
            match st.interp_mode {
                2 => codec::interp_none(&mut st.interp, &mut st.current, st.rate, &mut self.frame, &mut pos),
                // 0 = polifásica: o Citra também usa a linear [H: a polifásica real não é conhecida]
                _ => codec::interp_linear(&mut st.interp, &mut st.current, st.rate, &mut self.frame, &mut pos),
            }
            st.current_sample += (before - st.current.len()) as u32;
        }
        self.state.filters.process(&mut self.frame);
    }

    /// Tira o próximo buffer da fila (menor `buffer_id`) e decodifica-o inteiro em `current`.
    fn dequeue(&mut self, mem: &dyn SampleMemory) -> bool {
        let st = &mut self.state;
        debug_assert!(st.current.is_empty());
        let Some(idx) = (0..st.queue.len()).min_by_key(|&i| (st.queue[i].buffer_id, i)) else {
            return false;
        };
        let mut buf = st.queue.remove(idx);
        if trace() {
            eprintln!("[dsp-trace] fonte {} toca buffer id {} pa {:#x} len {} loop {} pos {}", self.id, buf.buffer_id, buf.pa, buf.length, buf.is_looping, if buf.has_played { 0 } else { buf.play_position });
        }
        if buf.adpcm_dirty {
            st.adpcm.yn1 = buf.adpcm_yn[0] as i16;
            st.adpcm.yn2 = buf.adpcm_yn[1] as i16;
        }
        let ch = channels(buf.mono_or_stereo);
        let n = buf.length as usize;
        // a máscara do endereço vem da configuração do DMA do DSP
        let Some(data) = mem.read(buf.pa & !3, codec::bytes_needed(buf.format, ch, n)) else {
            eprintln!(
                "[dsp] fonte {} buffer {} ({} amostras): endereço físico inválido {:#010x}",
                self.id, buf.buffer_id, buf.length, buf.pa
            );
            st.current.clear();
            return true;
        };
        st.current = match buf.format {
            FMT_PCM8 => codec::decode_pcm8(ch, &data, n),
            FMT_PCM16 => codec::decode_pcm16(ch, &data, n),
            FMT_ADPCM => codec::decode_adpcm(&data, n, &st.adpcm_coeffs, &mut st.adpcm),
            f => {
                eprintln!("[dsp] fonte {}: formato {f} desconhecido", self.id);
                codec::Buffer::new()
            }
        };
        // a primeira passada começa em play_position; os loops, do início
        st.current_sample = if buf.has_played { 0 } else { buf.play_position };
        st.current_pa = buf.pa;
        st.current_length = buf.length;
        st.current_mono_or_stereo = buf.mono_or_stereo;
        st.current_format = buf.format;
        st.current_is_looping = buf.is_looping;
        st.current_buffer_id = buf.buffer_id;
        st.last_buffer_id = 0;
        st.buffer_update = buf.from_queue && !buf.has_played;
        if buf.is_looping {
            buf.has_played = true;
            st.queue.push(buf);
        }
        let skip = (st.current_sample as usize).min(st.current.len());
        st.current.drain(..skip);
        true
    }

    fn current_status(&mut self) -> Status {
        let st = &mut self.state;
        let s = Status {
            is_enabled: st.enabled,
            buffer_id_dirty: st.buffer_update,
            sync_count: st.sync_count,
            buffer_position: st.current_sample,
            current_buffer_id: st.current_buffer_id,
            last_buffer_id: st.last_buffer_id,
        };
        st.buffer_update = false;
        s
    }

    /// Há amostras sendo tocadas neste quadro?
    pub fn playing(&self) -> bool {
        self.state.enabled && (!self.state.current.is_empty() || !self.state.queue.is_empty())
    }
}
