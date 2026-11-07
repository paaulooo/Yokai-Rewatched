//! Layout da memória compartilhada do DSP, igual ao HLE do Citra/Azahar (`audio_core/hle/shared_memory.h`).
//!
//! A RAM do DSP (512 KB) aparece para o ARM em 0x1FF00000. Há duas regiões de 0x8000 bytes, em +0x50000 e +0x70000,
//! usadas como buffer duplo: o DSP lê a que tem o `frame_counter` (último u16 da região) mais novo e escreve os
//! status na outra. O jogo pede ao DSP os endereços das 15 estruturas (palavras de 16 bits do DSP) pelo pipe de
//! áudio e converte com `ConvertProcessAddressFromDspDram`: `va = (a << 1) + 0x1FF40000`; a segunda região é
//! `a | 0x10000`.
//!
//! O DSP é de 16 bits: os campos u32 "do DSP" ficam com as metades trocadas (middle-endian) — ver [`rd32d`].
//! Floats e as amostras intermediárias (s32) são little-endian normais.

/// Início das duas regiões dentro da RAM do DSP.
pub const REGION0: usize = 0x5_0000;
pub const REGION1: usize = 0x7_0000;
pub const REGION_SIZE: usize = 0x8000;

pub const NUM_SOURCES: usize = 24;
pub const SAMPLES_PER_FRAME: usize = 160;

// Tamanhos das estruturas (bytes), na ordem em que aparecem na região.
pub const PAD0_SIZE: usize = 0x400 * 2;
pub const DSP_STATUS_SIZE: usize = 32;
pub const DSP_DEBUG_SIZE: usize = 0x260;
pub const FINAL_SAMPLES_SIZE: usize = SAMPLES_PER_FRAME * 2 * 2;
pub const SOURCE_STATUS_ONE: usize = 12;
pub const SOURCE_STATUS_SIZE: usize = NUM_SOURCES * SOURCE_STATUS_ONE;
pub const COMPRESSOR_SIZE: usize = 0xD20 * 2;
pub const DSP_CONFIG_SIZE: usize = 196;
pub const INTERMEDIATE_SIZE: usize = 2 * 4 * SAMPLES_PER_FRAME * 4;
pub const SOURCE_CONFIG_ONE: usize = 192;
pub const SOURCE_CONFIG_SIZE: usize = NUM_SOURCES * SOURCE_CONFIG_ONE;
pub const ADPCM_COEFFS_SIZE: usize = NUM_SOURCES * 16 * 2;
pub const UNK10_SIZE: usize = 0x100 * 2;
pub const UNK11_SIZE: usize = 0xC0 * 2;
pub const UNK12_SIZE: usize = 0x180 * 2;
pub const UNK13_SIZE: usize = 0xA * 2;
pub const UNK14_SIZE: usize = 0x13A3 * 2;

// Deslocamentos (bytes) dentro de uma região — acumulados a partir dos tamanhos, como o `struct SharedMemory`.
pub const DSP_STATUS: usize = PAD0_SIZE;
pub const DSP_DEBUG: usize = DSP_STATUS + DSP_STATUS_SIZE;
pub const FINAL_SAMPLES: usize = DSP_DEBUG + DSP_DEBUG_SIZE;
pub const SOURCE_STATUS: usize = FINAL_SAMPLES + FINAL_SAMPLES_SIZE;
pub const COMPRESSOR: usize = SOURCE_STATUS + SOURCE_STATUS_SIZE;
pub const DSP_CONFIG: usize = COMPRESSOR + COMPRESSOR_SIZE;
pub const INTERMEDIATE: usize = DSP_CONFIG + DSP_CONFIG_SIZE;
pub const SOURCE_CONFIG: usize = INTERMEDIATE + INTERMEDIATE_SIZE;
pub const ADPCM_COEFFS: usize = SOURCE_CONFIG + SOURCE_CONFIG_SIZE;
pub const UNK10: usize = ADPCM_COEFFS + ADPCM_COEFFS_SIZE;
pub const UNK11: usize = UNK10 + UNK10_SIZE;
pub const UNK12: usize = UNK11 + UNK11_SIZE;
pub const UNK13: usize = UNK12 + UNK12_SIZE;
pub const UNK14: usize = UNK13 + UNK13_SIZE;
pub const FRAME_COUNTER: usize = UNK14 + UNK14_SIZE;

/// Endereço DSP (palavras de 16 bits) de um deslocamento da região 0.
pub const fn dsp_addr(off: usize) -> u16 {
    (0x8000 + off / 2) as u16
}

/// Os 15 endereços na ordem em que o pipe de áudio os entrega (`AudioPipeWriteStructAddresses`).
pub const STRUCT_ADDRESSES: [u16; 15] = [
    dsp_addr(FRAME_COUNTER),
    dsp_addr(SOURCE_CONFIG),
    dsp_addr(SOURCE_STATUS),
    dsp_addr(ADPCM_COEFFS),
    dsp_addr(DSP_CONFIG),
    dsp_addr(DSP_STATUS),
    dsp_addr(FINAL_SAMPLES),
    dsp_addr(INTERMEDIATE),
    dsp_addr(COMPRESSOR),
    dsp_addr(DSP_DEBUG),
    dsp_addr(UNK10),
    dsp_addr(UNK11),
    dsp_addr(UNK12),
    dsp_addr(UNK13),
    dsp_addr(UNK14),
];

/// Campos de `SourceConfiguration::Configuration` (192 bytes).
pub mod src {
    pub const DIRTY: usize = 0; // u32 LE (bitfield)
    pub const GAIN: usize = 4; // f32[3][4]
    pub const RATE: usize = 52; // f32
    pub const INTERP: usize = 56; // u8
    pub const FILTERS_ENABLED: usize = 58; // u16
    pub const SIMPLE_B0: usize = 60;
    pub const SIMPLE_A1: usize = 62;
    pub const BIQUAD_A2: usize = 64;
    pub const BIQUAD_A1: usize = 66;
    pub const BIQUAD_B2: usize = 68;
    pub const BIQUAD_B1: usize = 70;
    pub const BIQUAD_B0: usize = 72;
    pub const BUFFERS_DIRTY: usize = 74; // u16
    pub const BUFFERS: usize = 76; // Buffer[4]
    pub const BUFFER_SIZE: usize = 20;
    pub const LOOP_RELATED: usize = 156; // u32 DSP
    pub const ENABLE: usize = 160; // u8
    pub const SYNC_COUNT: usize = 162; // u16
    pub const PLAY_POSITION: usize = 164; // u32 DSP
    pub const PHYS_ADDR: usize = 172; // u32 DSP
    pub const LENGTH: usize = 176; // u32 DSP
    pub const FLAGS1: usize = 180; // u16: bits 0-1 mono/estéreo, 2-3 formato, 5 fade-in
    pub const ADPCM_PS: usize = 182;
    pub const ADPCM_YN: usize = 184; // u16[2]
    pub const FLAGS2: usize = 188; // u16: bit 0 adpcm_dirty, bit 1 is_looping
    pub const BUFFER_ID: usize = 190;

    /// Campos de `Buffer` (20 bytes), relativos ao início do buffer.
    pub mod buf {
        pub const PHYS_ADDR: usize = 0;
        pub const LENGTH: usize = 4;
        pub const ADPCM_PS: usize = 8;
        pub const ADPCM_YN: usize = 10;
        pub const ADPCM_DIRTY: usize = 14;
        pub const IS_LOOPING: usize = 15;
        pub const BUFFER_ID: usize = 16;
    }

    // Bits de `dirty`.
    pub const FORMAT_DIRTY: u32 = 1 << 0;
    pub const MONO_STEREO_DIRTY: u32 = 1 << 1;
    pub const ADPCM_COEFFS_DIRTY: u32 = 1 << 2;
    pub const PARTIAL_EMBEDDED_DIRTY: u32 = 1 << 3;
    pub const PARTIAL_RESET: u32 = 1 << 4;
    pub const ENABLE_DIRTY: u32 = 1 << 16;
    pub const INTERP_DIRTY: u32 = 1 << 17;
    pub const RATE_DIRTY: u32 = 1 << 18;
    pub const BUFFER_QUEUE_DIRTY: u32 = 1 << 19;
    pub const LOOP_RELATED_DIRTY: u32 = 1 << 20;
    pub const PLAY_POSITION_DIRTY: u32 = 1 << 21;
    pub const FILTERS_ENABLED_DIRTY: u32 = 1 << 22;
    pub const SIMPLE_FILTER_DIRTY: u32 = 1 << 23;
    pub const BIQUAD_FILTER_DIRTY: u32 = 1 << 24;
    pub const GAIN0_DIRTY: u32 = 1 << 25;
    pub const SYNC_COUNT_DIRTY: u32 = 1 << 28;
    pub const RESET: u32 = 1 << 29;
    pub const EMBEDDED_DIRTY: u32 = 1 << 30;
}

/// Campos de `SourceStatus::Status` (12 bytes).
pub mod status {
    pub const IS_ENABLED: usize = 0; // u8
    pub const BUFFER_ID_DIRTY: usize = 1; // u8
    pub const SYNC_COUNT: usize = 2; // u16
    pub const BUFFER_POSITION: usize = 4; // u32 DSP
    pub const CURRENT_BUFFER_ID: usize = 8; // u16
    pub const LAST_BUFFER_ID: usize = 10; // u16
}

/// Campos de `DspConfiguration` (196 bytes).
pub mod cfg {
    pub const DIRTY: usize = 0; // u32 LE
    pub const MASTER_VOLUME: usize = 4; // f32
    pub const AUX_RETURN_VOLUME: usize = 8; // f32[2]
    pub const OUTPUT_BUFFER_COUNT: usize = 16;
    pub const OUTPUT_FORMAT: usize = 22; // u16: 0 mono, 1 estéreo, 2 surround
    pub const CLIPPING_MODE: usize = 24;
    pub const HEADPHONES: usize = 26;
    pub const AUX_FRONT_BYPASS: usize = 36; // u16[2]
    pub const AUX_BUS_ENABLE: usize = 40; // u16[2]
    pub const DELAY: usize = 44; // DelayEffect[2]
    pub const DELAY_SIZE: usize = 20;
    pub const REVERB: usize = 84; // ReverbEffect[2], 52 bytes cada
    pub const REVERB_SIZE: usize = 52;
    pub const SYNC_MODE: usize = 0xBC;
    pub const DIRTY2: usize = 0xC0;

    /// `DelayEffect` (20 bytes), relativo ao início do efeito.
    pub mod delay {
        pub const DIRTY: usize = 0; // u16
        pub const ENABLE: usize = 2;
        pub const OUTPUTS: usize = 6;
        pub const WORK_BUFFER: usize = 8; // u32 DSP
        pub const FRAME_COUNT: usize = 12;
        pub const G: usize = 14; // s16, 7 bits fracionários
        pub const A: usize = 16;
        pub const B: usize = 18;
    }

    pub const AUX_BUS_ENABLE0_DIRTY: u32 = 1 << 8;
    pub const AUX_BUS_ENABLE1_DIRTY: u32 = 1 << 9;
    pub const DELAY0_DIRTY: u32 = 1 << 10;
    pub const REVERB0_DIRTY: u32 = 1 << 12;
    pub const MASTER_VOLUME_DIRTY: u32 = 1 << 16;
    pub const AUX_RETURN_VOLUME0_DIRTY: u32 = 1 << 24;
    pub const OUTPUT_FORMAT_DIRTY: u32 = 1 << 26;
    pub const HEADPHONES_DIRTY: u32 = 1 << 28;
}

// ------------------------------------------------------------ acesso a bytes

pub fn rd8(b: &[u8], o: usize) -> u8 {
    b[o]
}
pub fn rd16(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
pub fn rd32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}
/// u32 do DSP: as duas metades de 16 bits trocadas em relação ao ARM.
pub fn rd32d(b: &[u8], o: usize) -> u32 {
    rd32(b, o).rotate_left(16)
}
pub fn rdf32(b: &[u8], o: usize) -> f32 {
    f32::from_bits(rd32(b, o))
}
pub fn wr16(b: &mut [u8], o: usize, v: u16) {
    b[o..o + 2].copy_from_slice(&v.to_le_bytes());
}
pub fn wr32(b: &mut [u8], o: usize, v: u32) {
    b[o..o + 4].copy_from_slice(&v.to_le_bytes());
}
pub fn wr32d(b: &mut [u8], o: usize, v: u32) {
    wr32(b, o, v.rotate_left(16));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Endereços DSP da tabela de `shared_memory.h` (região 0) — o layout tem de dar exatamente estes.
    #[test]
    fn struct_addresses_match_citra() {
        assert_eq!(dsp_addr(DSP_STATUS), 0x8400);
        assert_eq!(dsp_addr(DSP_DEBUG), 0x8410);
        assert_eq!(dsp_addr(FINAL_SAMPLES), 0x8540);
        assert_eq!(dsp_addr(SOURCE_STATUS), 0x8680);
        assert_eq!(dsp_addr(COMPRESSOR), 0x8710);
        assert_eq!(dsp_addr(DSP_CONFIG), 0x9430);
        assert_eq!(dsp_addr(INTERMEDIATE), 0x9492);
        assert_eq!(dsp_addr(SOURCE_CONFIG), 0x9E92);
        assert_eq!(dsp_addr(ADPCM_COEFFS), 0xA792);
        assert_eq!(dsp_addr(UNK10), 0xA912);
        assert_eq!(dsp_addr(UNK11), 0xAA12);
        assert_eq!(dsp_addr(UNK12), 0xAAD2);
        assert_eq!(dsp_addr(UNK13), 0xAC52);
        assert_eq!(dsp_addr(UNK14), 0xAC5C);
        assert_eq!(dsp_addr(FRAME_COUNTER), 0xBFFF);
        // ordem do pipe: frame counter, config. das fontes, status das fontes, coeficientes ADPCM, …
        assert_eq!(&STRUCT_ADDRESSES[..6], &[0xBFFF, 0x9E92, 0x8680, 0xA792, 0x9430, 0x8400]);
    }

    /// `ASSERT_DSP_STRUCT` e `static_assert` do Citra.
    #[test]
    fn struct_sizes_match_citra() {
        assert_eq!(FRAME_COUNTER + 2, REGION_SIZE, "SharedMemory = 0x8000");
        assert_eq!(SOURCE_CONFIG_ONE, 192);
        assert_eq!(src::BUFFER_SIZE, 20);
        assert_eq!(src::BUFFERS + 4 * src::BUFFER_SIZE, src::LOOP_RELATED);
        assert_eq!(src::BUFFER_ID + 2, SOURCE_CONFIG_ONE);
        assert_eq!(SOURCE_STATUS_ONE, 12);
        assert_eq!(status::LAST_BUFFER_ID + 2, SOURCE_STATUS_ONE);
        assert_eq!(DSP_CONFIG_SIZE, 196);
        assert_eq!(cfg::DELAY_SIZE, 20);
        assert_eq!(cfg::REVERB_SIZE, 52);
        assert_eq!(cfg::REVERB + 2 * cfg::REVERB_SIZE, cfg::SYNC_MODE);
        assert_eq!(cfg::DELAY + 2 * cfg::DELAY_SIZE, cfg::REVERB);
        assert_eq!(cfg::SYNC_MODE, 0xBC);
        assert_eq!(cfg::DIRTY2, 0xC0);
        assert_eq!(cfg::DIRTY2 + 4, DSP_CONFIG_SIZE);
        assert_eq!(ADPCM_COEFFS_SIZE, 768);
        assert_eq!(DSP_STATUS_SIZE, 32);
        assert_eq!(FINAL_SAMPLES_SIZE, 640);
        assert_eq!(INTERMEDIATE_SIZE, 5120);
        assert_eq!(DSP_DEBUG_SIZE, 0x260);
        // DspMemory: região 0 em 0x50000, região 1 em 0x70000, total 0x80000
        assert_eq!(REGION0 + REGION_SIZE + 0x18000, REGION1);
        assert_eq!(REGION1 + REGION_SIZE + 0x8000, 0x8_0000);
        // todas as estruturas alinhadas a 2 bytes
        for o in [DSP_STATUS, DSP_DEBUG, FINAL_SAMPLES, SOURCE_STATUS, COMPRESSOR, DSP_CONFIG, INTERMEDIATE,
            SOURCE_CONFIG, ADPCM_COEFFS, UNK10, UNK11, UNK12, UNK13, UNK14, FRAME_COUNTER]
        {
            assert_eq!(o % 2, 0);
        }
    }

    #[test]
    fn middle_endian_u32() {
        let mut b = [0u8; 4];
        wr32d(&mut b, 0, 0x1234_5678);
        assert_eq!(b, [0x34, 0x12, 0x78, 0x56]);
        assert_eq!(rd32d(&b, 0), 0x1234_5678);
    }
}
