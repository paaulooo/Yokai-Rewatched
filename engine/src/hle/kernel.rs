//! Kernel do 3DS em HLE: objetos (eventos, mutexes, semáforos, árbitro de endereços, threads, memória
//! compartilhada, sessões de serviço), escalonador e as syscalls que o `code.bin` usa.
//!
//! Threads guest = threads do SO, mas só uma roda por vez (a que tem a "CPU", `running`). A troca acontece nas
//! syscalls que bloqueiam (espera, sono, árbitro) e quando uma syscall acorda uma thread de prioridade maior
//! (número menor), como no núcleo de aplicativo do 3DS. Sem preempção no meio do código guest. [H: até agora o jogo
//! sempre espera por syscall no laço principal; se aparecer espera ativa, colocar um ponto de checagem nos laços]
//!
//! Convenções das syscalls (registradores) como no 3dbrew ("SVC") e no libctru.

use super::mem;
use std::cell::Cell;
use std::collections::BTreeMap;
use std::sync::{Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};
use yw1_rt::Cpu;

pub const RESULT_OK: u32 = 0;
pub const RESULT_TIMEOUT: u32 = 0x0940_1bfe;
pub const RESULT_INVALID_HANDLE: u32 = 0xd8e0_07f7;
pub const RESULT_NOT_FOUND: u32 = 0xd88007fa;

/// Pseudo-handles do kernel
pub const CUR_THREAD: u32 = 0xffff_8000;
pub const CUR_PROCESS: u32 = 0xffff_8001;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Reset {
    OneShot,
    Sticky,
    Pulse,
}

pub enum Obj {
    Event { signaled: bool, reset: Reset, name: &'static str },
    Mutex { owner: Option<usize>, count: u32 },
    Semaphore { count: i32, max: i32 },
    Timer { signaled: bool, reset: Reset, deadline: Option<Instant>, interval: Duration },
    Thread { tid: usize },
    Arbiter,
    /// bloco de memória: endereço guest de onde ele mora (os serviços escrevem ali) e tamanho
    SharedMem { addr: u32, size: u32, name: &'static str },
    /// sessão cliente de um serviço (`srv:GetServiceHandle`)
    Session { service: String },
    Process,
    ResourceLimit,
}

#[derive(Clone, Debug, PartialEq)]
enum TState {
    Ready,
    Running,
    Waiting,
    Dead,
}

#[derive(Clone)]
enum WaitOn {
    Objects { objs: Vec<usize>, all: bool },
    Arbiter { addr: u32 },
    Sleep,
}

struct Thread {
    prio: i32,
    state: TState,
    wait: Option<WaitOn>,
    deadline: Option<Instant>,
    /// resultado ao acordar: (código, índice do objeto)
    wake: (u32, i32),
    /// endereço da TLS (para depuração)
    #[allow(dead_code)]
    tls: u32,
    /// ordem de chegada na fila de prontos (FIFO entre iguais)
    seq: u64,
    handle_obj: usize,
    /// CPU guest desta thread (para o diagnóstico; válido enquanto a thread vive)
    cpu: usize,
    entry: u32,
}

pub struct State {
    objs: Vec<Option<Obj>>,
    handles: BTreeMap<u32, usize>,
    next_handle: u32,
    threads: Vec<Thread>,
    running: Option<usize>,
    /// desde quando nenhuma thread guest roda (console ocioso, esperando VBlank/evento)
    idle_since: Option<Instant>,
    seq: u64,
    start: Option<Instant>,
}

pub struct Kernel {
    st: Mutex<State>,
    cv: Condvar,
}

pub static K: Kernel = Kernel {
    st: Mutex::new(State {
        objs: Vec::new(),
        handles: BTreeMap::new(),
        next_handle: 0x10,
        threads: Vec::new(),
        running: None,
        idle_since: None,
        seq: 0,
        start: None,
    }),
    cv: Condvar::new(),
};

thread_local! {
    static ME: Cell<usize> = const { Cell::new(usize::MAX) };
    /// CPU guest da thread atual (para o diagnóstico de falhas)
    pub static CPU: Cell<*const Cpu> = const { Cell::new(std::ptr::null()) };
}

pub fn current() -> usize {
    ME.with(|m| m.get())
}

fn trace() -> bool {
    static T: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *T.get_or_init(|| std::env::var_os("YW_TRACE_SVC").is_some())
}

impl State {
    pub fn add_obj(&mut self, o: Obj) -> usize {
        self.objs.push(Some(o));
        self.objs.len() - 1
    }
    pub fn new_handle(&mut self, obj: usize) -> u32 {
        let h = self.next_handle;
        self.next_handle += 1;
        self.handles.insert(h, obj);
        h
    }
    pub fn create(&mut self, o: Obj) -> u32 {
        let id = self.add_obj(o);
        self.new_handle(id)
    }
    pub fn obj_of(&self, h: u32) -> Option<usize> {
        match h {
            CUR_THREAD => Some(self.threads[current()].handle_obj),
            _ => self.handles.get(&h).copied(),
        }
    }
    pub fn obj(&mut self, id: usize) -> Option<&mut Obj> {
        self.objs.get_mut(id).and_then(|o| o.as_mut())
    }
    pub fn handle_obj(&mut self, h: u32) -> Option<&mut Obj> {
        let id = self.obj_of(h)?;
        self.obj(id)
    }

    /// O objeto está sinalizado para `tid`? (sem consumir)
    fn ready_for(&self, id: usize, tid: usize) -> bool {
        match self.objs.get(id).and_then(|o| o.as_ref()) {
            Some(Obj::Event { signaled, .. }) | Some(Obj::Timer { signaled, .. }) => *signaled,
            Some(Obj::Mutex { owner, .. }) => owner.is_none() || *owner == Some(tid),
            Some(Obj::Semaphore { count, .. }) => *count > 0,
            Some(Obj::Thread { tid: t }) => self.threads.get(*t).is_some_and(|t| t.state == TState::Dead),
            _ => false,
        }
    }

    /// Consome o sinal (evento one-shot, trava o mutex, decrementa o semáforo).
    fn acquire(&mut self, id: usize, tid: usize) {
        match self.obj(id) {
            Some(Obj::Event { signaled, reset, .. }) | Some(Obj::Timer { signaled, reset, .. }) => {
                if *reset == Reset::OneShot {
                    *signaled = false;
                }
            }
            Some(Obj::Mutex { owner, count }) => {
                *owner = Some(tid);
                *count += 1;
            }
            Some(Obj::Semaphore { count, .. }) => *count -= 1,
            _ => {}
        }
    }

    /// Reavalia as threads esperando: quem pode acordar fica pronta.
    pub fn wake_waiters(&mut self) {
        for tid in 0..self.threads.len() {
            if self.threads[tid].state != TState::Waiting {
                continue;
            }
            let Some(WaitOn::Objects { objs, all }) = self.threads[tid].wait.clone() else {
                continue;
            };
            if all {
                if objs.iter().all(|&o| self.ready_for(o, tid)) {
                    for &o in &objs {
                        self.acquire(o, tid);
                    }
                    self.make_ready(tid, RESULT_OK, 0);
                }
            } else if let Some(i) = objs.iter().position(|&o| self.ready_for(o, tid)) {
                self.acquire(objs[i], tid);
                self.make_ready(tid, RESULT_OK, i as i32);
            }
        }
        // eventos "pulse" só acordam quem já estava esperando
        for o in self.objs.iter_mut().flatten() {
            if let Obj::Event { signaled, reset: Reset::Pulse, .. } = o {
                *signaled = false;
            }
        }
    }

    fn make_ready(&mut self, tid: usize, res: u32, idx: i32) {
        let t = &mut self.threads[tid];
        t.state = TState::Ready;
        t.wait = None;
        t.deadline = None;
        t.wake = (res, idx);
        self.seq += 1;
        t.seq = self.seq;
    }

    /// Melhor thread pronta (menor prioridade numérica; FIFO).
    fn pick(&self) -> Option<usize> {
        self.threads
            .iter()
            .enumerate()
            .filter(|(_, t)| t.state == TState::Ready)
            .min_by_key(|(_, t)| (t.prio, t.seq))
            .map(|(i, _)| i)
    }

    /// Acorda quem passou do prazo.
    fn expire(&mut self, now: Instant) {
        for tid in 0..self.threads.len() {
            let t = &self.threads[tid];
            if t.state == TState::Waiting && t.deadline.is_some_and(|d| d <= now) {
                let res = if matches!(t.wait, Some(WaitOn::Sleep)) {
                    RESULT_OK
                } else {
                    RESULT_TIMEOUT
                };
                self.make_ready(tid, res, -1);
            }
        }
        // timers
        let mut fired = false;
        for o in self.objs.iter_mut().flatten() {
            if let Obj::Timer { signaled, deadline, interval, .. } = o {
                if deadline.is_some_and(|d| d <= now) {
                    *signaled = true;
                    fired = true;
                    *deadline = if interval.is_zero() { None } else { Some(now + *interval) };
                }
            }
        }
        if fired {
            self.wake_waiters();
        }
    }

    fn next_deadline(&self) -> Option<Instant> {
        let t = self
            .threads
            .iter()
            .filter(|t| t.state == TState::Waiting)
            .filter_map(|t| t.deadline);
        let tm = self.objs.iter().flatten().filter_map(|o| match o {
            Obj::Timer { deadline, .. } => *deadline,
            _ => None,
        });
        t.chain(tm).min()
    }
}

impl Kernel {
    pub fn lock(&self) -> MutexGuard<'_, State> {
        self.st.lock().unwrap()
    }

    /// Bloqueia a thread atual com `wait` (ou só cede a vez, se `wait` for None) até ela voltar a ter a CPU.
    /// Devolve (resultado, índice).
    fn block<'a>(&'a self, mut g: MutexGuard<'a, State>, wait: Option<WaitOn>, deadline: Option<Instant>) -> (MutexGuard<'a, State>, (u32, i32)) {
        let me = current();
        match wait {
            Some(w) => {
                g.threads[me].state = TState::Waiting;
                g.threads[me].wait = Some(w);
                g.threads[me].deadline = deadline;
            }
            None => {
                g.seq += 1;
                let s = g.seq;
                g.threads[me].state = TState::Ready;
                g.threads[me].seq = s;
            }
        }
        g.running = None;
        g.idle_since = Some(Instant::now());
        self.schedule(&mut g);
        loop {
            if g.running == Some(me) {
                break;
            }
            // ninguém rodando: quem esperar o próximo prazo cuida dele
            let timeout = g.next_deadline().map(|d| d.saturating_duration_since(console_now()));
            g = match timeout {
                Some(t) if g.running.is_none() => self.cv.wait_timeout(g, t.max(Duration::from_micros(50))).unwrap().0,
                _ => self.cv.wait(g).unwrap(),
            };
            if g.running.is_none() {
                g.expire(console_now());
                self.schedule(&mut g);
            }
        }
        let r = g.threads[me].wake;
        (g, r)
    }

    /// Dá a CPU para a melhor thread pronta, se ninguém estiver rodando.
    pub fn schedule(&self, g: &mut State) {
        if g.running.is_some() {
            return;
        }
        if let Some(t) = g.pick() {
            g.threads[t].state = TState::Running;
            g.running = Some(t);
            if let Some(t0) = g.idle_since.take() {
                IDLE_NS.fetch_add(t0.elapsed().as_nanos() as u64, std::sync::atomic::Ordering::Relaxed);
            }
            self.cv.notify_all();
        }
    }

    /// Depois de sinalizar algo: se acordou uma thread mais prioritária que a atual, cede a vez.
    pub fn maybe_preempt<'a>(&'a self, g: MutexGuard<'a, State>) -> MutexGuard<'a, State> {
        let me = current();
        let better = g.pick().is_some_and(|t| g.threads[t].prio < g.threads[me].prio);
        if better {
            self.block(g, None, None).0
        } else {
            g
        }
    }

    /// Evento externo (interrupção do GSP, VBlank…) sinalizado por uma thread do host.
    pub fn signal_external(&self, obj: usize) {
        let mut g = self.lock();
        if let Some(Obj::Event { signaled, .. }) = g.obj(obj) {
            *signaled = true;
        }
        g.wake_waiters();
        self.schedule(&mut g);
        // acordou alguém mais prioritário que a thread que está rodando: o código gerado cede a vez no próximo ponto
        // de checagem (salto para trás / entrada de função), como a preempção do núcleo de aplicativo do 3DS
        if let Some(r) = g.running {
            let rp = g.threads[r].prio;
            if g.threads.iter().any(|t| t.state == TState::Ready && t.prio < rp) {
                yw1_rt::PREEMPT.store(true, std::sync::atomic::Ordering::Relaxed);
            }
        }
    }

    /// Ponto de preempção do código guest (`rt::poll`): se há uma thread pronta mais prioritária, cede a vez.
    pub fn preempt_point(&self) {
        yw1_rt::PREEMPT.store(false, std::sync::atomic::Ordering::Relaxed);
        let me = current();
        if me == usize::MAX {
            return;
        }
        let g = self.lock();
        if g.running != Some(me) {
            return;
        }
        drop(self.maybe_preempt(g));
    }

    /// Registra a thread guest atual (chamado pela própria thread do SO antes de rodar código guest).
    fn enter(&self, tid: usize) {
        ME.with(|m| m.set(tid));
        let mut g = self.lock();
        self.schedule(&mut g);
        while g.running != Some(tid) {
            g = self.cv.wait(g).unwrap();
        }
    }
}

/// Cria a thread principal e devolve o id; `run` executa o código guest a partir de `entry`.
pub fn spawn_thread(entry: u32, arg: u32, stack_top: u32, prio: i32) -> (usize, u32) {
    let mut g = K.lock();
    let tid = g.threads.len();
    let tls = mem::TLS + tid as u32 * mem::TLS_SIZE;
    let obj = g.add_obj(Obj::Thread { tid });
    let h = g.new_handle(obj);
    g.seq += 1;
    let seq = g.seq;
    g.threads.push(Thread {
        prio,
        state: TState::Ready,
        wait: None,
        deadline: None,
        wake: (0, 0),
        tls,
        seq,
        handle_obj: obj,
        cpu: 0,
        entry,
    });
    drop(g);
    std::thread::Builder::new()
        .name(format!("guest-{tid}"))
        // o código recompilado usa a pilha do host para as chamadas guest
        .stack_size(256 << 20)
        .spawn(move || {
            K.enter(tid);
            let mut c = Cpu {
                tls,
                ..Default::default()
            };
            c.r[0] = arg;
            c.r[13] = stack_top;
            c.r[14] = 0;
            CPU.with(|p| p.set(&c as *const Cpu));
            K.lock().threads[tid].cpu = &c as *const Cpu as usize;
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| yw1_rt::call(&mut c, entry)));
            if let Err(e) = r {
                let msg = e
                    .downcast_ref::<String>()
                    .cloned()
                    .or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string()))
                    .unwrap_or_default();
                eprintln!("[hle] thread {tid} parou: {msg}\n{}", dump(&c));
                super::crashed(msg);
            }
            exit_thread();
        })
        .expect("criar thread do host");
    (tid, h)
}

/// Estado de todas as threads guest (diagnóstico de travamentos).
/// Diagnóstico de travamento: thread em execução, função guest atual e registradores (leitura sem parar a thread).
pub fn running_snapshot() -> String {
    let g = K.lock();
    let Some(tid) = g.running else { return "nenhuma thread rodando".into() };
    let t = &g.threads[tid];
    if t.cpu == 0 {
        return format!("t{tid} sem Cpu");
    }
    // SAFETY: Cpu da thread viva; leitura concorrente só para diagnóstico
    let c = unsafe { &*(t.cpu as *const Cpu) };
    format!("t{tid} prio {} função {:#x} r={:08x?}", t.prio, c.func, &c.r)
}

/// (thread, prioridade, função guest atual, registradores) da thread em execução — só para diagnóstico (vigia).
pub fn running_regs() -> Option<(usize, i32, u32, [u32; 16])> {
    let g = K.lock();
    let tid = g.running?;
    let t = &g.threads[tid];
    if t.cpu == 0 {
        return None;
    }
    // SAFETY: Cpu da thread viva; leitura concorrente só para diagnóstico
    let c = unsafe { &*(t.cpu as *const Cpu) };
    Some((tid, t.prio as i32, c.func, c.r))
}

pub fn threads_report() -> String {
    let g = K.lock();
    let mut s = String::new();
    for (tid, t) in g.threads.iter().enumerate() {
        let wait = match &t.wait {
            Some(WaitOn::Objects { objs, all }) => {
                let names: Vec<String> = objs
                    .iter()
                    .map(|&o| match g.objs.get(o).and_then(|x| x.as_ref()) {
                        Some(Obj::Event { name, signaled, .. }) => format!("evento {name} (sinal={signaled})"),
                        Some(Obj::Mutex { owner, .. }) => format!("mutex (dono={owner:?})"),
                        Some(Obj::Semaphore { count, .. }) => format!("semáforo ({count})"),
                        Some(Obj::Thread { tid }) => format!("thread {tid}"),
                        Some(Obj::Timer { .. }) => "timer".into(),
                        _ => format!("obj {o}"),
                    })
                    .collect();
                format!("espera {} [{}]", if *all { "todos" } else { "um de" }, names.join(", "))
            }
            Some(WaitOn::Arbiter { addr }) => format!("árbitro em {addr:#x}"),
            Some(WaitOn::Sleep) => "dormindo".into(),
            None => String::new(),
        };
        // SAFETY: ponteiro do Cpu da thread viva (só leitura para diagnóstico)
        let (func, lr, sp) = if t.cpu != 0 {
            let c = unsafe { &*(t.cpu as *const Cpu) };
            (c.func, c.r[14], c.r[13])
        } else {
            (0, 0, 0)
        };
        // pilha guest: endereços de retorno prováveis (palavras que apontam para .text)
        let mut stack = Vec::new();
        if sp != 0 {
            for i in 0..64 {
                let v = yw1_rt::r32(sp + 4 * i);
                if (0x10_0000..0x4b_fca0).contains(&(v & !1)) && stack.len() < 8 {
                    stack.push(format!("{v:#x}"));
                }
            }
        }
        s.push_str(&format!(
            "  t{tid} entrada {:#x} prio {} {:?} função {func:#x} lr {lr:#x} {wait} · pilha [{}]\n",
            t.entry,
            t.prio,
            t.state,
            stack.join(" ")
        ));
    }
    s
}

pub fn dump(c: &Cpu) -> String {
    let mut s = String::new();
    for i in 0..16 {
        s.push_str(&format!("r{i}={:08x} ", c.r[i]));
        if i % 8 == 7 {
            s.push('\n');
        }
    }
    s.push_str(&format!("nzcv={}{}{}{}", c.n as u8, c.z as u8, c.c as u8, c.v as u8));
    s
}

fn exit_thread() {
    let mut g = K.lock();
    let me = current();
    g.threads[me].state = TState::Dead;
    g.running = None;
    g.wake_waiters();
    K.schedule(&mut g);
}

/// Tempo total (ns) em que nenhuma thread guest rodava — o resto é código do jogo + HLE + renderização.
pub static IDLE_NS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// "Agora" do console: o relógio do host menos o tempo em que o menu pausou o jogo. Prazos (sleep, timeouts) e a
/// expiração usam este relógio, então param junto com a pausa e não disparam todos de uma vez ao voltar.
pub fn console_now() -> Instant {
    let now = Instant::now();
    now.checked_sub(Duration::from_nanos(crate::hle::paused_ns())).unwrap_or(now)
}

pub fn ns_to_deadline(ns: i64) -> Option<Instant> {
    if ns < 0 {
        None
    } else {
        // relógio do console acelerado nos testes (`YW_SPEED`): o tempo guest passa `speed` vezes mais rápido
        Some(console_now() + Duration::from_nanos((ns as f64 / crate::hle::speed()) as u64))
    }
}

/// Ticks do relógio do sistema (268.111.856 Hz no 3DS).
pub fn system_tick() -> u64 {
    let mut g = K.lock();
    let t0 = *g.start.get_or_insert_with(Instant::now);
    let run = t0.elapsed().saturating_sub(Duration::from_nanos(crate::hle::paused_ns()));
    (run.as_secs_f64() * crate::hle::speed() * 268_111_856.0) as u64
}


/// Tratamento das syscalls (chamado por `yw1_svc`).
pub static DIAG_SVC: [std::sync::atomic::AtomicU32; 64] = [const { std::sync::atomic::AtomicU32::new(0) }; 64];
pub fn diag_running() -> (Option<usize>, u32, bool) {
    let g = K.lock();
    let r = g.running;
    let t8w = g.threads.get(8).is_some_and(|t| t.state == TState::Waiting && matches!(t.wait, Some(WaitOn::Objects{..})));
    (r, r.map_or(0, |t| DIAG_SVC[t].load(std::sync::atomic::Ordering::Relaxed)), t8w)
}
struct DiagGuard(usize);
impl Drop for DiagGuard { fn drop(&mut self) { DIAG_SVC[self.0].store(0, std::sync::atomic::Ordering::Relaxed); } }
pub fn svc(c: &mut Cpu, n: u32) {
    DIAG_SVC[current()].store(n + 0x100 * (if n == 0x32 { 1 } else { 0 }) + 1, std::sync::atomic::Ordering::Relaxed);
    let _dg = DiagGuard(current());
    if trace() {
        eprintln!(
            "[svc] t{} {n:#04x} r0={:08x} r1={:08x} r2={:08x} r3={:08x} lr={:08x}",
            current(),
            c.r[0],
            c.r[1],
            c.r[2],
            c.r[3],
            c.r[14]
        );
    }
    match n {
        // ControlMemory(op, addr0, addr1, size, perm) → r1 = endereço
        0x01 => {
            let (op, addr0, size) = (c.r[0], c.r[1], c.r[3]);
            match op & 0xff {
                3 => {
                    let a = if op & 0x10000 != 0 {
                        mem::linear_commit(size)
                    } else {
                        mem::heap_commit(addr0, size)
                    };
                    c.r[1] = a;
                    c.r[0] = RESULT_OK;
                }
                // FREE: mantém o commit [H] (só conta, para o mapa de memória)
                1 => {
                    mem::free(addr0, size);
                    c.r[0] = RESULT_OK;
                }
                6 => c.r[0] = RESULT_OK, // PROTECT
                _ => {
                    eprintln!("[svc] ControlMemory op {op:#x} não tratado");
                    c.r[0] = RESULT_OK;
                }
            }
        }
        0x03 => {
            eprintln!("[hle] o jogo chamou ExitProcess");
            super::crashed("ExitProcess".into());
            loop {
                std::thread::park();
            }
        }
        // CreateThread(prio, entry, arg, stack_top, core) → r1 = handle
        0x08 => {
            let (prio, entry, arg, top) = (c.r[0] as i32, c.r[1], c.r[2], c.r[3]);
            let (_tid, h) = spawn_thread(entry, arg, top, prio);
            c.r[0] = RESULT_OK;
            c.r[1] = h;
            let g = K.lock();
            drop(K.maybe_preempt(g));
        }
        0x09 => {
            exit_thread();
            loop {
                std::thread::park();
            }
        }
        // SleepThread(ns em r0:r1)
        0x0a => {
            let ns = (c.r[0] as u64 | (c.r[1] as u64) << 32) as i64;
            let g = K.lock();
            if ns <= 0 {
                drop(K.block(g, None, None));
            } else {
                drop(K.block(g, Some(WaitOn::Sleep), ns_to_deadline(ns)));
            }
        }
        // SetThreadPriority(handle, prio)
        0x0c => {
            let mut g = K.lock();
            if let Some(Obj::Thread { tid }) = g.obj_of(c.r[0]).and_then(|o| g.objs[o].as_ref()) {
                let tid = *tid;
                g.threads[tid].prio = c.r[1] as i32;
            }
            c.r[0] = RESULT_OK;
            drop(K.maybe_preempt(g));
        }
        // CreateMutex(_, locked) → r1
        0x13 => {
            let mut g = K.lock();
            let me = current();
            let locked = c.r[1] != 0;
            c.r[1] = g.create(Obj::Mutex {
                owner: locked.then_some(me),
                count: locked as u32,
            });
            c.r[0] = RESULT_OK;
        }
        0x14 => {
            let mut g = K.lock();
            if let Some(Obj::Mutex { owner, count }) = g.handle_obj(c.r[0]) {
                *count = count.saturating_sub(1);
                if *count == 0 {
                    *owner = None;
                }
            }
            g.wake_waiters();
            c.r[0] = RESULT_OK;
            drop(K.maybe_preempt(g));
        }
        // CreateSemaphore(_, initial, max) → r1
        0x15 => {
            let mut g = K.lock();
            c.r[1] = g.create(Obj::Semaphore {
                count: c.r[1] as i32,
                max: c.r[2] as i32,
            });
            c.r[0] = RESULT_OK;
        }
        // ReleaseSemaphore(_, handle, n) → r1 = contagem anterior
        0x16 => {
            let mut g = K.lock();
            let mut prev = 0;
            if let Some(Obj::Semaphore { count, max }) = g.handle_obj(c.r[1]) {
                prev = *count;
                *count = (*count + c.r[2] as i32).min(*max);
            }
            g.wake_waiters();
            c.r[0] = RESULT_OK;
            c.r[1] = prev as u32;
            drop(K.maybe_preempt(g));
        }
        // CreateEvent(_, reset) → r1
        0x17 => {
            let mut g = K.lock();
            let reset = match c.r[1] {
                0 => Reset::OneShot,
                1 => Reset::Sticky,
                _ => Reset::Pulse,
            };
            c.r[1] = g.create(Obj::Event {
                signaled: false,
                reset,
                name: "guest",
            });
            c.r[0] = RESULT_OK;
        }
        0x18 => {
            let mut g = K.lock();
            if let Some(Obj::Event { signaled, .. }) = g.handle_obj(c.r[0]) {
                *signaled = true;
            }
            g.wake_waiters();
            c.r[0] = RESULT_OK;
            drop(K.maybe_preempt(g));
        }
        0x19 => {
            let mut g = K.lock();
            if let Some(Obj::Event { signaled, .. }) | Some(Obj::Timer { signaled, .. }) = g.handle_obj(c.r[0]) {
                *signaled = false;
            }
            c.r[0] = RESULT_OK;
        }
        // CreateTimer(_, reset) → r1
        0x1a => {
            let mut g = K.lock();
            let reset = match c.r[1] {
                0 => Reset::OneShot,
                1 => Reset::Sticky,
                _ => Reset::Pulse,
            };
            c.r[1] = g.create(Obj::Timer {
                signaled: false,
                reset,
                deadline: None,
                interval: Duration::ZERO,
            });
            c.r[0] = RESULT_OK;
        }
        // SetTimer(handle, initial ns r2:r3, interval ns r1:r4)
        0x1b => {
            let mut g = K.lock();
            let initial = (c.r[2] as u64 | (c.r[3] as u64) << 32) as i64;
            let interval = (c.r[1] as u64 | (c.r[4] as u64) << 32) as i64;
            if let Some(Obj::Timer { deadline, interval: iv, .. }) = g.handle_obj(c.r[0]) {
                *deadline = ns_to_deadline(initial.max(0));
                *iv = Duration::from_nanos(interval.max(0) as u64);
            }
            c.r[0] = RESULT_OK;
        }
        // CreateMemoryBlock(other_perm, addr, size, my_perm) → r1
        0x1e => {
            let mut g = K.lock();
            let (addr, size) = (c.r[1], c.r[2]);
            c.r[1] = g.create(Obj::SharedMem {
                addr,
                size,
                name: "guest",
            });
            c.r[0] = RESULT_OK;
        }
        // MapMemoryBlock(handle, addr, my_perm, other_perm)
        0x1f => {
            let mut g = K.lock();
            let want = c.r[1];
            let mut res = RESULT_OK;
            match g.handle_obj(c.r[0]) {
                Some(Obj::SharedMem { addr, size, name }) => {
                    if *addr == 0 {
                        // bloco de serviço ainda sem endereço: mora onde o jogo pedir
                        let a = if want != 0 { want } else { next_shared(*size) };
                        mem::map(a, *size, name);
                        *addr = a;
                        super::services::shared_mapped(name, a);
                    } else if want != 0 && want != *addr {
                        eprintln!("[svc] MapMemoryBlock {name} pedido em {want:#x}, já está em {addr:#x} [H: usar o endereço já mapeado]");
                    }
                }
                _ => res = RESULT_INVALID_HANDLE,
            }
            c.r[0] = res;
        }
        0x20 => c.r[0] = RESULT_OK,
        0x21 => {
            let mut g = K.lock();
            c.r[1] = g.create(Obj::Arbiter);
            c.r[0] = RESULT_OK;
        }
        // ArbitrateAddress(handle, addr, type, value, timeout r4:r5)
        0x22 => arbitrate(c),
        // CloseHandle
        0x23 => {
            let mut g = K.lock();
            g.handles.remove(&c.r[0]);
            c.r[0] = RESULT_OK;
        }
        // WaitSynchronization1(handle, timeout r2:r3)
        0x24 => {
            let ns = (c.r[2] as u64 | (c.r[3] as u64) << 32) as i64;
            let (res, _) = wait(&[c.r[0]], false, ns);
            c.r[0] = res;
        }
        // WaitSynchronizationN(timeout lo, handles*, n, all, timeout hi) → r1 = índice
        0x25 => {
            let ns = (c.r[0] as u64 | (c.r[4] as u64) << 32) as i64;
            let n = c.r[2] as usize;
            let hs: Vec<u32> = (0..n).map(|i| yw1_rt::r32(c.r[1] + 4 * i as u32)).collect();
            let (res, idx) = wait(&hs, c.r[3] != 0, ns);
            c.r[0] = res;
            c.r[1] = idx as u32;
        }
        // DuplicateHandle(_, handle) → r1
        0x27 => {
            let mut g = K.lock();
            match g.obj_of(c.r[1]) {
                Some(o) => {
                    c.r[1] = g.new_handle(o);
                    c.r[0] = RESULT_OK;
                }
                None => c.r[0] = RESULT_INVALID_HANDLE,
            }
        }
        0x28 => {
            let t = system_tick();
            c.r[0] = t as u32;
            c.r[1] = (t >> 32) as u32;
        }
        // GetSystemInfo / GetProcessInfo: respostas mínimas
        0x2a | 0x2b => {
            c.r[0] = RESULT_OK;
            c.r[1] = 0;
            c.r[2] = 0;
        }
        // ConnectToPort(_, nome*) → r1
        0x2d => {
            let name = read_cstr(c.r[1], 12);
            let mut g = K.lock();
            c.r[1] = g.create(Obj::Session { service: name });
            c.r[0] = RESULT_OK;
        }
        0x32 => {
            let h = c.r[0];
            c.r[0] = super::services::request(c, h);
        }
        0x35 => {
            c.r[1] = 0x28; // id de processo de aplicativo típico [H]
            c.r[0] = RESULT_OK;
        }
        0x37 => {
            let g = K.lock();
            let h = c.r[1];
            let tid = match g.obj_of(h).and_then(|o| g.objs[o].as_ref()) {
                Some(Obj::Thread { tid }) => *tid as u32,
                _ => current() as u32,
            };
            c.r[1] = 0x100 + tid;
            c.r[0] = RESULT_OK;
        }
        0x38 => {
            let mut g = K.lock();
            c.r[1] = g.create(Obj::ResourceLimit);
            c.r[0] = RESULT_OK;
        }
        // GetResourceLimitLimitValues (0x39) / CurrentValues (0x3a)(values*, handle, names*, n): 0 prioridade,
        // 1 memória, 2 threads … (valores do modo de aplicativo de 64 MB) [H nos demais]
        0x39 | 0x3a => {
            let (out, names, cnt) = (c.r[0], c.r[2], c.r[3]);
            for i in 0..cnt {
                let name = yw1_rt::r32(names + 4 * i);
                let v: u64 = match (n, name) {
                    (0x39, 0) => 0x18,
                    (0x39, 1) => mem::APP_MEMORY as u64,
                    (0x39, _) => 32,
                    (_, 1) => mem::used() as u64,
                    _ => 0,
                };
                yw1_rt::w32(out + 8 * i, v as u32);
                yw1_rt::w32(out + 8 * i + 4, (v >> 32) as u32);
            }
            c.r[0] = RESULT_OK;
        }
        0x3c => {
            eprintln!("[hle] svcBreak({:#x}) em lr={:08x}\n{}", c.r[0], c.r[14], dump(c));
        }
        // OutputDebugString(str*, len)
        0x3d => {
            eprintln!("[guest] {}", read_cstr(c.r[0], c.r[1] as usize));
        }
        _ => {
            eprintln!("[svc] syscall {n:#04x} não implementada (lr={:08x})", c.r[14]);
            c.r[0] = 0xf800_1bfe;
        }
    }
}

fn next_shared(size: u32) -> u32 {
    static NEXT: Mutex<u32> = Mutex::new(mem::SHARED);
    let mut n = NEXT.lock().unwrap();
    let a = *n;
    *n += (size + 0xfff) & !0xfff;
    a
}

pub fn read_cstr(a: u32, max: usize) -> String {
    let mut s = Vec::new();
    for i in 0..max as u32 {
        let b = yw1_rt::r8(a + i) as u8;
        if b == 0 {
            break;
        }
        s.push(b);
    }
    String::from_utf8_lossy(&s).into_owned()
}

/// WaitSynchronization: devolve (resultado, índice).
pub fn wait(hs: &[u32], all: bool, ns: i64) -> (u32, i32) {
    let mut g = K.lock();
    let me = current();
    let mut objs = Vec::new();
    for &h in hs {
        match g.obj_of(h) {
            Some(o) => objs.push(o),
            None => return (RESULT_INVALID_HANDLE, -1),
        }
    }
    // já sinalizado?
    if all {
        if objs.iter().all(|&o| g.ready_for(o, me)) {
            for &o in &objs {
                g.acquire(o, me);
            }
            return (RESULT_OK, 0);
        }
    } else if let Some(i) = objs.iter().position(|&o| g.ready_for(o, me)) {
        g.acquire(objs[i], me);
        return (RESULT_OK, i as i32);
    }
    if ns == 0 {
        return (RESULT_TIMEOUT, -1);
    }
    let (_g, r) = K.block(g, Some(WaitOn::Objects { objs, all }), ns_to_deadline(ns));
    r
}

fn arbitrate(c: &mut Cpu) {
    let (addr, ty, value) = (c.r[1], c.r[2], c.r[3] as i32);
    let ns = (c.r[4] as u64 | (c.r[5] as u64) << 32) as i64;
    let mut g = K.lock();
    let me = current();
    match ty {
        // SIGNAL: acorda `value` threads (-1 = todas) esperando no endereço, por prioridade
        0 => {
            let mut waiting: Vec<usize> = (0..g.threads.len())
                .filter(|&t| {
                    g.threads[t].state == TState::Waiting
                        && matches!(g.threads[t].wait, Some(WaitOn::Arbiter { addr: a }) if a == addr)
                })
                .collect();
            waiting.sort_by_key(|&t| (g.threads[t].prio, g.threads[t].seq));
            let n = if value < 0 { waiting.len() } else { (value as usize).min(waiting.len()) };
            for &t in &waiting[..n] {
                g.make_ready(t, RESULT_OK, 0);
            }
            c.r[0] = RESULT_OK;
            drop(K.maybe_preempt(g));
        }
        1..=4 => {
            let cur = yw1_rt::r32(addr) as i32;
            if cur < value {
                if ty == 2 || ty == 4 {
                    yw1_rt::w32(addr, (cur - 1) as u32);
                }
                let deadline = if ty >= 3 { ns_to_deadline(ns) } else { None };
                let (_g, (res, _)) = K.block(g, Some(WaitOn::Arbiter { addr }), deadline);
                c.r[0] = res;
            } else {
                c.r[0] = RESULT_OK;
            }
            let _ = me;
        }
        _ => c.r[0] = RESULT_OK,
    }
}
