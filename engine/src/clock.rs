//! Relógio do jogo: contador próprio do jogo, como no 3DS (não segue o relógio do PC).
//!
//! O que o `code.bin` faz (lido no export do Ghidra):
//! - O relógio do jogo é um contador `u32` em `[DAT_005c69f8+0x10]+0xc`, em **terços de segundo**:
//!   hora = t / 10800, minuto = (t / 180) % 60, segundo = (t / 3) % 60 (`FUN_002b3810`, usado por `op14041(i)`).
//!   `op14041(3)` devolve 0 de dia (06:00–17:59) e 3 à noite; índices ≥ 4 devolvem o contador cru (`FUN_002b38f4`).
//! - `op14040(h, m)` grava `h·10800 + m·180` (`FUN_002aec6c` → `FUN_002aec10`).
//! - `FUN_00371b44` avança o contador a cada quadro (`+ dt·3`; passando de 23h volta a 0) **exceto** com o flag
//!   `time_lock` (crc32 `0x2a329223`) ligado (`op14000('time_lock', 1)`).
//!
//! No port (motor congelado): o contador anda em tempo real enquanto o jogo roda; novo jogo parte de 0 [H: valor
//! inicial não medido — a história ajusta com `op14040`]. A data do diário (`op12055`) continua vindo do relógio do
//! sistema, como o RTC do console.

use std::sync::Mutex;
use std::time::Instant;

/// Data/hora local do sistema (equivale ao RTC do console; usada só na data do diário).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LocalTime {
    pub year: u16,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
}

impl LocalTime {
    /// Campos na ordem do registro de data do save (`op12055`): ano, mês, dia, hora, minuto.
    pub fn save_fields(&self) -> [u16; 5] {
        [
            self.year,
            self.month as u16,
            self.day as u16,
            self.hour as u16,
            self.minute as u16,
        ]
    }
}

#[cfg(windows)]
pub fn local_now() -> LocalTime {
    #[repr(C)]
    struct SystemTime {
        year: u16,
        month: u16,
        day_of_week: u16,
        day: u16,
        hour: u16,
        minute: u16,
        second: u16,
        milliseconds: u16,
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn GetLocalTime(t: *mut SystemTime);
    }
    let mut t = SystemTime {
        year: 0,
        month: 0,
        day_of_week: 0,
        day: 0,
        hour: 0,
        minute: 0,
        second: 0,
        milliseconds: 0,
    };
    // SAFETY: GetLocalTime só escreve na estrutura SYSTEMTIME passada.
    unsafe { GetLocalTime(&mut t) };
    LocalTime {
        year: t.year,
        month: t.month as u8,
        day: t.day as u8,
        hour: t.hour as u8,
        minute: t.minute as u8,
        second: t.second as u8,
    }
}

/// Fora do Windows: UTC (sem fuso; o alvo do port é Windows).
#[cfg(not(windows))]
pub fn local_now() -> LocalTime {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let days = (secs / 86400) as i64;
    let rem = secs % 86400;
    // conversão civil (Howard Hinnant)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u8;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u8;
    let year = (yoe + era * 400 + if month <= 2 { 1 } else { 0 }) as u16;
    LocalTime {
        year,
        month,
        day,
        hour: (rem / 3600) as u8,
        minute: (rem / 60 % 60) as u8,
        second: (rem % 60) as u8,
    }
}

const DAY_UNITS: u32 = 24 * 10800;

struct State {
    units: u32,
    since: Option<Instant>,
    lock: bool,
}

static STATE: Mutex<State> = Mutex::new(State {
    units: 0,
    since: None,
    lock: false,
});

fn units_of(h: u32, m: u32) -> u32 {
    (h * 10800 + m * 180) % DAY_UNITS
}

/// Aplica o tempo decorrido desde a última leitura (`FUN_00371b44`).
fn advance(st: &mut State) {
    let now = Instant::now();
    if let Some(t0) = st.since {
        if !st.lock {
            let add = (now.duration_since(t0).as_secs_f64() * 3.0) as u32;
            if add > 0 {
                st.units = st.units.wrapping_add(add);
                if st.units / 10800 > 23 {
                    st.units = 0;
                }
                // guarda a fração que sobrou
                st.since = Some(t0 + std::time::Duration::from_secs_f64(add as f64 / 3.0));
                return;
            }
            return;
        }
    }
    st.since = Some(now);
}

/// Contador do relógio do jogo (terços de segundo desde 00:00).
pub fn game_units() -> u32 {
    let mut st = STATE.lock().unwrap();
    advance(&mut st);
    st.units
}

/// `op14041(i)`: 0 hora, 1 minuto, 2 segundo, 3 período (0 dia, 3 noite), ≥4 contador cru.
pub fn query(i: i32) -> i32 {
    let t = game_units();
    (match i {
        0 => t / 10800,
        1 => t / 180 % 60,
        2 => t / 3 % 60,
        3 => {
            if (t / 10800).wrapping_sub(6) < 12 {
                0
            } else {
                3
            }
        }
        _ => t,
    }) as i32
}

/// `op14040(h, m)`
pub fn set_game_time(h: i32, m: i32) {
    let mut st = STATE.lock().unwrap();
    st.units = units_of(h.max(0) as u32, m.max(0) as u32);
    st.since = Some(Instant::now());
}

/// `op14000('time_lock', v)`: 1 congela, 0 volta a andar.
pub fn set_lock(on: bool) {
    let mut st = STATE.lock().unwrap();
    advance(&mut st);
    st.lock = on;
    st.since = Some(Instant::now());
}

/// Novo jogo: contador em 0, destravado.
pub fn reset() {
    let mut st = STATE.lock().unwrap();
    *st = State {
        units: 0,
        since: Some(Instant::now()),
        lock: false,
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn game_clock_is_the_games_own_counter() {
        reset();
        assert_eq!(query(0), 0);
        let pc = local_now();
        assert!((1..=12).contains(&pc.month) && (1..=31).contains(&pc.day) && pc.year >= 2024);
        // a história força 19:30 → noite
        set_game_time(19, 30);
        assert_eq!((query(0), query(1), query(3)), (19, 30, 3));
        // congelada com time_lock
        set_lock(true);
        set_game_time(7, 30);
        std::thread::sleep(std::time::Duration::from_millis(400));
        assert_eq!((query(0), query(1), query(3), query(4)), (7, 30, 0, 7 * 10800 + 30 * 180));
        // destravada, anda 3 unidades por segundo
        set_lock(false);
        std::thread::sleep(std::time::Duration::from_millis(700));
        assert!(query(4) >= 7 * 10800 + 30 * 180 + 2);
        // passou de 23:59 → 0
        set_game_time(23, 59);
        STATE.lock().unwrap().units = DAY_UNITS - 1;
        STATE.lock().unwrap().since = Some(Instant::now() - std::time::Duration::from_secs(1));
        assert_eq!(query(0), 0);
        reset();
    }
}
