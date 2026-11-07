//! APT (gerenciador de applets): ciclo de vida do aplicativo.
//! Comandos como no 3dbrew "NS and APT Services". Só o caminho de um aplicativo que roda sozinho: Initialize →
//! Enable → o evento de parâmetro dispara com `Wakeup` → ReceiveParameter devolve o sinal 1.

use super::{new_event, new_shared, signal, unknown, Ipc};
use crate::hle::kernel::{Obj, Reset, K};
use std::sync::Mutex;

struct Apt {
    lock: u32,
    notif: (u32, usize),
    param: (u32, usize),
    /// sinal pendente para ReceiveParameter (1 = Wakeup)
    pending: Option<u32>,
    font: u32,
}

static APT: Mutex<Option<Apt>> = Mutex::new(None);

fn with<R>(f: impl FnOnce(&mut Apt) -> R) -> R {
    let mut g = APT.lock().unwrap();
    let a = g.get_or_insert_with(|| {
        let lock = K.lock().create(Obj::Mutex { owner: None, count: 0 });
        Apt {
            lock,
            notif: new_event(Reset::OneShot, "apt-notif"),
            param: new_event(Reset::OneShot, "apt-param"),
            pending: None,
            font: new_shared("font", 0x0033_2000),
        }
    });
    f(a)
}

pub fn request(ipc: &Ipc) {
    match ipc.cmd() {
        // GetLockHandle(flags) → atributos, estado, handle do mutex
        0x0001 => {
            let h = with(|a| a.lock);
            ipc.ok(3, 2);
            ipc.set(2, 0);
            ipc.set(3, 0);
            ipc.set(4, 0);
            ipc.set(5, h);
        }
        // Initialize(app_id, atributos) → eventos de notificação e de parâmetro
        0x0002 => {
            let (n, p) = with(|a| (a.notif.0, a.param.0));
            ipc.ok(1, 3);
            ipc.set(2, 0x0400_0000);
            ipc.set(3, n);
            ipc.set(4, p);
        }
        // Enable: o aplicativo recebe o Wakeup
        0x0003 => {
            let id = with(|a| {
                a.pending = Some(1);
                a.param.1
            });
            signal(id);
            ipc.ok(1, 0);
        }
        // GetAppletManInfo
        0x0005 => {
            ipc.ok(5, 0);
            ipc.set(2, 0);
            ipc.set(3, 0x300); // aplicativo atual
            ipc.set(4, 0x300);
            ipc.set(5, 0x101);
        }
        // IsRegistered(app_id) → sim
        0x0009 => {
            ipc.ok(2, 0);
            ipc.set(2, 1);
        }
        // InquireNotification → nenhuma
        0x000b => {
            ipc.ok(2, 0);
            ipc.set(2, 0);
        }
        // ReceiveParameter / GlanceParameter(app_id, tamanho) → remetente, sinal, tamanho do buffer
        0x000d | 0x000e => {
            let sig = with(|a| {
                if ipc.cmd() == 0x000d {
                    a.pending.take()
                } else {
                    a.pending
                }
            });
            ipc.ok(4, 4);
            ipc.set(2, 0);
            ipc.set(3, sig.unwrap_or(0));
            ipc.set(4, 0);
            ipc.set(5, 0);
            ipc.set(6, 0);
            ipc.set(7, 0);
            ipc.set(8, 0);
        }
        // CancelParameter
        0x000f => {
            ipc.ok(2, 0);
            ipc.set(2, 1);
        }
        // PrepareToCloseApplication / CloseApplication / ReplySleepQuery / NotifyToWait / SetScreenCapPostPermission…
        0x0022 | 0x0027 | 0x003e | 0x0043 | 0x0055 | 0x0056 | 0x0058 => ipc.ok(1, 0),
        // GetSharedFont → endereço e handle do bloco (fonte do sistema; o jogo usa as próprias fontes) [H]
        0x0044 => {
            let h = with(|a| a.font);
            ipc.ok(2, 2);
            ipc.set(2, 0x1800_0000);
            ipc.set(3, 0);
            ipc.set(4, h);
        }
        // AppletUtility
        0x004b => {
            ipc.ok(2, 0);
            ipc.set(2, 0);
        }
        // SetApplicationCpuTimeLimit / GetApplicationCpuTimeLimit
        0x004f => ipc.ok(1, 0),
        0x0050 => {
            ipc.ok(2, 0);
            ipc.set(2, 30);
        }
        // CheckNew3DSApp / CheckNew3DS → não
        0x0101 | 0x0102 => {
            ipc.ok(2, 0);
            ipc.set(2, 0);
        }
        _ => unknown("APT:U", ipc),
    }
}
