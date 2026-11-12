//! Perfil de GPU por draw (diagnóstico; `YW_VK_DRAWPROF=arquivo.csv`). Para cada draw gravado (passada nativa e
//! sombra N×): timestamp TOP_OF_PIPE antes, BOTTOM_OF_PIPE depois e uma query de pipeline statistics (invocações do
//! shader de vértice, primitivas que chegaram ao recorte e invocações do shader de fragmento). Quando a GPU termina o
//! lote (`Vk::retire`, lotes em voo) as linhas vão para o CSV. Cada vaga do anel de envios tem a sua faixa de queries
//! e as suas linhas (um lote pode estar na GPU enquanto o seguinte grava). Desligado = nenhuma query, nenhum custo.
//!
//! Os timestamps de draws vizinhos se sobrepõem (a GPU executa vários ao mesmo tempo); a análise
//! (`tools/drawprof.py`) usa a diferença entre BOTTOM_OF_PIPE consecutivos do mesmo buffer de comandos como o custo
//! incremental de cada draw.
use ash::vk;
use std::io::Write;

/// draws por buffer de comandos e lote (o resto do lote fica sem medida; contado em `skipped`)
pub const CAP: u32 = 2048;

pub struct Meta {
    pub ns: u32,
    pub w: u32,
    pub h: u32,
    pub prims: u32,
    pub aot: bool,
    pub shaded: bool,
    pub dtest: bool,
    pub dwrite: bool,
    pub dfunc: u32,
    pub blend: bool,
    pub mask: u32,
    pub stencil: bool,
    pub aniso: u32,
    pub linear: u32,
    pub ntex: u32,
    pub wbuf: bool,
    pub alpha_test: bool,
    pub color: u32,
    pub pass_break: bool,
}

pub struct Prof {
    pub ts: vk::QueryPool,
    pub ps: vk::QueryPool,
    out: std::io::BufWriter<std::fs::File>,
    /// por vaga do anel e por buffer (0 = nativo, 1 = sombra): metadados dos draws gravados no lote da vaga, na ordem
    rows: Vec<[Vec<Meta>; 2]>,
    /// vaga do lote em gravação (`reset`)
    cur: usize,
    pub skipped: u64,
    batch: u64,
}

const PS_FLAGS: vk::QueryPipelineStatisticFlags = vk::QueryPipelineStatisticFlags::from_raw(
    vk::QueryPipelineStatisticFlags::VERTEX_SHADER_INVOCATIONS.as_raw()
        | vk::QueryPipelineStatisticFlags::CLIPPING_PRIMITIVES.as_raw()
        | vk::QueryPipelineStatisticFlags::FRAGMENT_SHADER_INVOCATIONS.as_raw(),
);

pub fn wanted() -> Option<String> {
    std::env::var("YW_VK_DRAWPROF").ok().filter(|v| !v.is_empty() && v != "0")
}

impl Prof {
    /// `slots` = vagas do anel de envios (uma faixa de queries por vaga).
    pub unsafe fn new(device: &ash::Device, path: &str, slots: usize) -> Result<Prof, String> {
        let k = slots.max(1) as u32;
        let ts = device
            .create_query_pool(&vk::QueryPoolCreateInfo::default().query_type(vk::QueryType::TIMESTAMP).query_count(4 * CAP * k), None)
            .map_err(|e| format!("drawprof ts: {e:?}"))?;
        let ps = device
            .create_query_pool(
                &vk::QueryPoolCreateInfo::default()
                    .query_type(vk::QueryType::PIPELINE_STATISTICS)
                    .query_count(2 * CAP * k)
                    .pipeline_statistics(PS_FLAGS),
                None,
            )
            .map_err(|e| format!("drawprof ps: {e:?}"))?;
        let f = std::fs::File::create(path).map_err(|e| format!("drawprof {path}: {e}"))?;
        let mut out = std::io::BufWriter::new(f);
        let _ = writeln!(
            out,
            "batch,cb,idx,t_top_us,t_bot_us,vs_inv,clip_prims,fs_inv,ns,w,h,prims,aot,shaded,dtest,dwrite,dfunc,blend,mask,stencil,aniso,linear,ntex,wbuf,atest,color,pass_break"
        );
        eprintln!("[vulkan] perfil por draw em {path}");
        Ok(Prof { ts, ps, out, rows: (0..k).map(|_| [Vec::new(), Vec::new()]).collect(), cur: 0, skipped: 0, batch: 0 })
    }

    /// Começo do lote da vaga `slot`, no buffer `pre` (executa antes dos outros no mesmo envio). As linhas do envio
    /// anterior dessa vaga já foram recolhidas (a vaga só é reusada depois que a GPU termina e `collect` roda).
    pub unsafe fn reset(&mut self, d: &ash::Device, pre: vk::CommandBuffer, slot: usize) {
        self.cur = slot;
        d.cmd_reset_query_pool(pre, self.ts, 4 * CAP * slot as u32, 4 * CAP);
        d.cmd_reset_query_pool(pre, self.ps, 2 * CAP * slot as u32, 2 * CAP);
        self.rows[slot][0].clear();
        self.rows[slot][1].clear();
    }

    /// Antes do draw no buffer `which`; devolve o índice da query ou None (sem vaga).
    pub unsafe fn before(&mut self, d: &ash::Device, cb: vk::CommandBuffer, which: usize) -> Option<u32> {
        let i = self.rows[self.cur][which].len() as u32;
        if i >= CAP {
            self.skipped += 1;
            return None;
        }
        let q = 2 * CAP * self.cur as u32 + which as u32 * CAP + i;
        d.cmd_write_timestamp(cb, vk::PipelineStageFlags::TOP_OF_PIPE, self.ts, 2 * q);
        d.cmd_begin_query(cb, self.ps, q, vk::QueryControlFlags::empty());
        Some(q)
    }

    pub unsafe fn after(&mut self, d: &ash::Device, cb: vk::CommandBuffer, which: usize, q: u32, m: Meta) {
        d.cmd_end_query(cb, self.ps, q);
        d.cmd_write_timestamp(cb, vk::PipelineStageFlags::BOTTOM_OF_PIPE, self.ts, 2 * q + 1);
        self.rows[self.cur][which].push(m);
    }

    /// Depois que a GPU terminou o lote da vaga `slot` (timeline): lê as queries e grava as linhas. `t0` = timestamp do
    /// começo do lote (ticks).
    pub unsafe fn collect(&mut self, d: &ash::Device, slot: usize, t0: u64, period_ns: f32) {
        self.batch += 1;
        for which in 0..2 {
            let n = self.rows[slot][which].len();
            if n == 0 {
                continue;
            }
            let base = 2 * CAP * slot as u32 + which as u32 * CAP;
            let mut ts = vec![0u64; 2 * n];
            let mut ps = vec![0u64; 3 * n];
            if d.get_query_pool_results(self.ts, 2 * base, &mut ts, vk::QueryResultFlags::TYPE_64).is_err() {
                continue;
            }
            // resultados de pipeline statistics: 3 contadores por query, na ordem dos bits
            let mut raw = vec![[0u64; 3]; n];
            if d.get_query_pool_results(self.ps, base, &mut raw, vk::QueryResultFlags::TYPE_64).is_err() {
                continue;
            }
            for (k, r) in raw.iter().enumerate() {
                ps[3 * k..3 * k + 3].copy_from_slice(r);
            }
            let us = |t: u64| if t >= t0 { (t - t0) as f64 * period_ns as f64 / 1000.0 } else { -1.0 };
            for (k, m) in self.rows[slot][which].iter().enumerate() {
                let _ = writeln!(
                    self.out,
                    "{},{},{},{:.2},{:.2},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{:08x},{}",
                    self.batch,
                    which,
                    k,
                    us(ts[2 * k]),
                    us(ts[2 * k + 1]),
                    ps[3 * k],
                    ps[3 * k + 1],
                    ps[3 * k + 2],
                    m.ns,
                    m.w,
                    m.h,
                    m.prims,
                    m.aot as u8,
                    m.shaded as u8,
                    m.dtest as u8,
                    m.dwrite as u8,
                    m.dfunc,
                    m.blend as u8,
                    m.mask,
                    m.stencil as u8,
                    m.aniso,
                    m.linear,
                    m.ntex,
                    m.wbuf as u8,
                    m.alpha_test as u8,
                    m.color,
                    m.pass_break as u8,
                );
            }
        }
        let _ = self.out.flush();
    }
}
