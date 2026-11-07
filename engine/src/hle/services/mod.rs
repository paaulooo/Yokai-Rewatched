//! Serviços do 3DS (HLE). `SendSyncRequest(handle)` chega aqui com o buffer de comando em `TLS + 0x80`.
//!
//! Formato IPC: palavra 0 = `(id << 16) | (normais << 6) | traduzidas`; a resposta põe o resultado na palavra 1.
//! Descritores de handle: `0x0000_0000` (copiar 1), `0x10` (mover 1), `(n-1) << 26` para n handles.
//! Comandos não tratados são registrados (uma vez cada) e respondidos com sucesso e saídas zeradas. [H por comando]

pub mod apt;
pub mod dsp;
pub mod fs;
pub mod gsp;
pub mod hid;
pub mod misc;
pub mod y2r;

use super::kernel::{self, Obj, K, RESULT_OK};
use std::collections::HashSet;
use std::sync::Mutex;
use yw1_rt::{r32, w32, Cpu};

/// Visão do buffer de comando da thread atual.
pub struct Ipc {
    pub base: u32,
}

impl Ipc {
    pub fn get(&self, i: u32) -> u32 {
        r32(self.base + 4 * i)
    }
    pub fn set(&self, i: u32, v: u32) {
        w32(self.base + 4 * i, v)
    }
    pub fn header(&self) -> u32 {
        self.get(0)
    }
    pub fn cmd(&self) -> u32 {
        self.get(0) >> 16
    }
    /// Escreve o cabeçalho de resposta e o resultado.
    pub fn reply(&self, normal: u32, translate: u32, result: u32) {
        self.set(0, self.cmd() << 16 | normal << 6 | translate);
        self.set(1, result);
    }
    pub fn ok(&self, normal: u32, translate: u32) {
        self.reply(normal, translate, RESULT_OK)
    }
    /// Buffer estático de recepção `i` (da área de 0x180 da TLS): endereço configurado pelo jogo.
    pub fn static_buf(&self, i: u32) -> u32 {
        r32(self.base + 0x100 + 8 * i + 4)
    }
}

static UNKNOWN: Mutex<Option<HashSet<(String, u32)>>> = Mutex::new(None);

pub fn unknown(service: &str, ipc: &Ipc) {
    let mut u = UNKNOWN.lock().unwrap();
    let set = u.get_or_insert_with(HashSet::new);
    if set.insert((service.to_string(), ipc.header())) {
        eprintln!(
            "[ipc] {service} comando {:#06x} (cabeçalho {:#010x}) sem tratamento: respondido OK com zeros",
            ipc.cmd(),
            ipc.header()
        );
    }
    ipc.ok(1, 0);
    for i in 2..8 {
        ipc.set(i, 0);
    }
}

fn trace() -> bool {
    static T: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *T.get_or_init(|| std::env::var_os("YW_TRACE_IPC").is_some())
}

/// Despacha uma requisição síncrona. Devolve o resultado da syscall (o do comando vai no buffer).
pub fn request(c: &mut Cpu, h: u32) -> u32 {
    let service = {
        let mut g = K.lock();
        match g.handle_obj(h) {
            Some(Obj::Session { service }) => service.clone(),
            _ => return kernel::RESULT_INVALID_HANDLE,
        }
    };
    let ipc = Ipc { base: c.tls + 0x80 };
    if trace() {
        eprintln!("[ipc] {service} {:#010x}", ipc.header());
    }
    match service.as_str() {
        "srv:" => srv(&ipc),
        "APT:U" | "APT:A" | "APT:S" => apt::request(&ipc),
        "fs:USER" => fs::request(&ipc, None),
        s if s.starts_with("fs:file:") || s.starts_with("fs:dir:") => fs::request(&ipc, Some(s)),
        "gsp::Gpu" => gsp::request(&ipc),
        "hid:USER" | "hid:SPVR" => hid::request(&ipc),
        "dsp::DSP" => dsp::request(&ipc),
        "cfg:u" | "cfg:s" | "cfg:i" => misc::cfg(&ipc),
        "ptm:u" | "ptm:sysm" => misc::ptm(&ipc),
        "y2r:u" => y2r::request(&ipc),
        "err:f" => {
            // ThrowFatalError(info): tipo (u8), …, resultado em +4, pc em +8 [3dbrew "ERR:f"]
            eprintln!(
                "[err:f] erro fatal do jogo: tipo {} resultado {:#010x} pc {:#010x} lr {:#010x}",
                ipc.get(1) & 0xff,
                ipc.get(2),
                ipc.get(3),
                ipc.get(4)
            );
            ipc.ok(1, 0);
        }
        "cecd:u" => misc::cecd(&ipc),
        "ndm:u" | "ac:u" | "boss:U" | "cam:u" | "ldr:ro" | "frd:u" | "ir:USER" | "ir:u" | "mic:u"
        | "news:u" | "nim:aoc" | "am:app" | "act:u" | "csnd:SND" | "ns:s" | "nwm::UDS" | "soc:U" => {
            misc::stub(&service, &ipc)
        }
        _ => unknown(&service, &ipc),
    }
    RESULT_OK
}

/// Gerenciador de serviços (`srv:`).
fn srv(ipc: &Ipc) {
    match ipc.cmd() {
        // RegisterClient
        0x0001 => ipc.ok(1, 0),
        // EnableNotification → semáforo
        0x0002 => {
            let h = K.lock().create(Obj::Semaphore { count: 0, max: 64 });
            ipc.ok(1, 2);
            ipc.set(2, 0);
            ipc.set(3, h);
        }
        // GetServiceHandle(nome[8], tamanho, flags)
        0x0005 => {
            let mut name = Vec::new();
            for i in 0..8u32 {
                let b = yw1_rt::r8(ipc.base + 4 + i) as u8;
                if b == 0 {
                    break;
                }
                name.push(b);
            }
            let name = String::from_utf8_lossy(&name).into_owned();
            let len = ipc.get(3) as usize;
            let name: String = name.chars().take(len.max(1)).collect();
            eprintln!("[ipc] srv:GetServiceHandle(\"{name}\")");
            let h = K.lock().create(Obj::Session { service: name });
            ipc.ok(1, 2);
            ipc.set(2, 0x10);
            ipc.set(3, h);
        }
        // Subscribe / Unsubscribe
        0x0009 | 0x000a => ipc.ok(1, 0),
        // ReceiveNotification → id 0 (nada)
        0x000b => {
            ipc.ok(2, 0);
            ipc.set(2, 0);
        }
        _ => unknown("srv:", ipc),
    }
}

/// Um bloco de memória compartilhada de serviço acabou de ganhar endereço (MapMemoryBlock).
pub fn shared_mapped(name: &str, addr: u32) {
    eprintln!("[hle] memória compartilhada {name} em {addr:#010x}");
    match name {
        "gsp" => gsp::mapped(addr),
        "hid" => hid::mapped(addr),
        _ => {}
    }
}

/// Cria um bloco de memória compartilhada de serviço (o endereço vem no MapMemoryBlock do jogo).
pub fn new_shared(name: &'static str, size: u32) -> u32 {
    K.lock().create(Obj::SharedMem { addr: 0, size, name })
}

/// Cria um evento do kernel e devolve (handle, id do objeto) — o id serve para sinalizar de fora.
pub fn new_event(reset: kernel::Reset, name: &'static str) -> (u32, usize) {
    let mut g = K.lock();
    let id = g.add_obj(Obj::Event {
        signaled: false,
        reset,
        name,
    });
    (g.new_handle(id), id)
}

/// Sinaliza um evento pelo id do objeto (de dentro de uma syscall: sem trocar de thread).
pub fn signal(id: usize) {
    let mut g = K.lock();
    if let Some(Obj::Event { signaled, .. }) = g.obj(id) {
        *signaled = true;
    }
    g.wake_waiters();
    K.schedule(&mut g);
}
