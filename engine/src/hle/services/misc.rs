//! Serviços menores: `cfg:u` (configuração do console), `ptm:u` (bateria/pedômetro), `y2r:u` (YUV→RGB) e stubs.

use super::{new_event, signal, unknown, Ipc};
use crate::hle::kernel::Reset;
use std::sync::Mutex;
use yw1_rt::{w32, w8};

pub fn cfg(ipc: &Ipc) {
    match ipc.cmd() {
        // GetConfigInfoBlk2(tamanho, bloco, desc, ptr)
        0x0001 => {
            let (size, block, ptr) = (ipc.get(1), ipc.get(2), ipc.get(4));
            let data = config_block(block, size);
            yw1_rt::write_bytes(ptr, &data);
            ipc.ok(1, 2);
            ipc.set(2, (size << 4) | 0xc);
            ipc.set(3, ptr);
        }
        // GetRegion → 1 (EUA)
        0x0002 => {
            ipc.ok(2, 0);
            ipc.set(2, 1);
        }
        // GenHashConsoleUnique → valor fixo
        0x0003 => {
            ipc.ok(3, 0);
            ipc.set(2, 0x1234_5678);
            ipc.set(3, 0x9abc);
        }
        // GetRegionCanadaUSA / GetSystemModel (0 = 3DS) / IsCoppacsSupported
        0x0004 | 0x0005 => {
            ipc.ok(2, 0);
            ipc.set(2, if ipc.cmd() == 0x0004 { 1 } else { 0 });
        }
        // GetModelNintendo2DS → 1 = não é 2DS
        0x0006 => {
            ipc.ok(2, 0);
            ipc.set(2, 1);
        }
        _ => unknown("cfg:u", ipc),
    }
}

/// Blocos de configuração (valores de um 3DS americano em inglês; 3dbrew "Config Savegame").
fn config_block(id: u32, size: u32) -> Vec<u8> {
    let mut d = vec![0u8; size as usize];
    let mut put = |bytes: &[u8]| {
        let n = bytes.len().min(d.len());
        d[..n].copy_from_slice(&bytes[..n]);
    };
    match id {
        // idioma: 1 = inglês
        0x000a_0002 => put(&[1]),
        // país: [?, ?, província, país 49 = EUA]
        0x000b_0000 => put(&[0, 0, 2, 49]),
        // nome do usuário (UTF-16)
        0x000a_0000 => {
            let name: Vec<u8> = "PC".encode_utf16().flat_map(|c| c.to_le_bytes()).collect();
            put(&name);
        }
        // aniversário: mês, dia
        0x000a_0001 => put(&[1, 1]),
        // saída de som: 1 = estéreo
        0x0007_0001 => put(&[1]),
        // câmera estéreo (8 floats)
        0x0005_0005 => {
            let v: Vec<u8> = [62.0f32, 289.0, 76.80, 46.08, 10.0, 5.0, 55.58, 21.57]
                .iter()
                .flat_map(|f| f.to_le_bytes())
                .collect();
            put(&v);
        }
        _ => {}
    }
    d
}

pub fn ptm(ipc: &Ipc) {
    match ipc.cmd() {
        // GetAdapterState / GetBatteryChargeState / GetPedometerState → 0
        0x0005 | 0x0008 | 0x0009 => {
            ipc.ok(2, 0);
            ipc.set(2, 0);
        }
        // GetShellState → aberto
        0x0006 => {
            ipc.ok(2, 0);
            ipc.set(2, 1);
        }
        // GetBatteryLevel → cheio
        0x0007 => {
            ipc.ok(2, 0);
            ipc.set(2, 5);
        }
        // GetStepHistory(horas, início lo/hi, desc, ptr) → zeros
        0x000b => {
            let (hours, ptr) = (ipc.get(1), ipc.get(5));
            for i in 0..hours {
                yw1_rt::w16(ptr + 2 * i, 0);
            }
            ipc.ok(1, 2);
            ipc.set(2, (hours * 2) << 4 | 0xc);
            ipc.set(3, ptr);
        }
        // GetTotalStepCount
        0x000c => {
            ipc.ok(2, 0);
            ipc.set(2, 0);
        }
        _ => unknown("ptm:u", ipc),
    }
}

static CECD_EVENT: Mutex<Option<u32>> = Mutex::new(None);

/// `cecd:u` (StreetPass) sem rede: estado sempre ocioso e eventos de mudança de estado já sinalizados.
/// Comandos (3dbrew "CECD Services"): 0x000C Start/Stop(comando), 0x000E GetCecStateAbbreviated,
/// 0x000F GetCecInfoEventHandle, 0x0010 GetChangeStateEventHandle.
pub fn cecd(ipc: &Ipc) {
    match ipc.cmd() {
        0x000e => {
            // 1 = CEC_STATE_ABBREV_IDLE
            ipc.ok(2, 0);
            ipc.set(2, 1);
        }
        0x000f | 0x0010 => {
            let h = *CECD_EVENT.lock().unwrap().get_or_insert_with(|| {
                let (h, id) = new_event(Reset::Sticky, "cecd");
                signal(id);
                h
            });
            ipc.ok(1, 2);
            ipc.set(2, 0);
            ipc.set(3, h);
        }
        _ => unknown("cecd:u", ipc),
    }
}

/// Serviços que o jogo abre mas que não afetam o jogo local (rede, câmera, StreetPass…): respondem OK.
pub fn stub(service: &str, ipc: &Ipc) {
    unknown(service, ipc);
    // alguns "Initialize" devolvem eventos: um handle 0 é aceito pelo jogo na maioria dos casos [H]
    let _ = (w8 as fn(u32, u32), w32 as fn(u32, u32));
}
