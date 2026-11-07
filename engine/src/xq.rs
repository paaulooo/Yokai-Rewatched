//! Interpretador dos scripts XQ32 da Level-5 (`seq/**/*.xq`). Formato em docs/FORMATS.md (seção XQ32);
//! porte do leitor de `tools/xq32_disasm.py`. Semântica das instruções: `work/notes/xq32_opcodes.md`
//! (catálogo) — o núcleo (fluxo, aritmética, chamadas, corrotinas) é executado aqui; os opcodes nativos do
//! motor (≥ 1000: objetos 2D, som, cenas, eventos...) vão para o `Host`.
//!
//! Modelo de variáveis (deduzido do uso nos scripts, HIPÓTESE coerente com todos os casos vistos):
//!   `$0..$999`    contexto da corrotina: na cena, os argumentos do `op2104(cena, volta, extras...)`; nos callbacks
//!                 de objeto (ex. `Menu_MatrixNormal`), os parâmetros do objeto (`op5046(obj, i, v)` → `$i`)
//!   `$1000`       valor de retorno (`return` = `op11`)    `$1001..$1999` temporários do quadro de função
//!   `$2000..$2999` locais da função                       `$3000..$3999` argumentos do `call`/`op2211`
//!   `$4000..`     globais do arquivo `.xq`
//! O destino de cada instrução recebe o resultado do opcode (para `call`, o valor de retorno da função).

use crate::assets::{crc32, l5, u16le, u32le, Result};
use std::collections::HashMap;
use std::rc::Rc;

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Int(i32),
    Float(f32),
    Str(Rc<str>),
    /// Argumento tipo 2: CRC-32 de um nome (função, rótulo, objeto).
    Hash(u32),
    /// Vetor/matriz (`op530(largura, altura)`); `op100(v, i, j)` grava, `op531(a, i, j)` lê.
    Array(Rc<std::cell::RefCell<(usize, Vec<Value>)>>),
}

impl Value {
    pub fn int(&self) -> i32 {
        match self {
            Value::Int(i) => *i,
            Value::Float(f) => *f as i32,
            Value::Hash(h) => *h as i32,
            Value::Str(s) => s.parse().unwrap_or(0),
            Value::Array(a) => a.borrow().1.len() as i32,
        }
    }

    pub fn float(&self) -> f32 {
        match self {
            Value::Float(f) => *f,
            other => other.int() as f32,
        }
    }

    pub fn truthy(&self) -> bool {
        match self {
            Value::Float(f) => *f != 0.0,
            Value::Str(s) => !s.is_empty(),
            other => other.int() != 0,
        }
    }

    /// Chave de nome: string e hash do mesmo nome são o mesmo objeto (os scripts usam os dois).
    pub fn key(&self) -> u32 {
        match self {
            Value::Str(s) => crc32(&raw_bytes(s)),
            Value::Hash(h) => *h,
            Value::Int(i) => *i as u32,
            Value::Float(f) => f.to_bits(),
            Value::Array(_) => 0,
        }
    }

    pub fn text(&self) -> String {
        match self {
            Value::Str(s) => s.to_string(),
            Value::Int(i) => i.to_string(),
            Value::Float(f) => format!("{f}"),
            Value::Hash(h) => format!("#{h:08x}"),
            Value::Array(a) => format!("[{} itens]", a.borrow().1.len()),
        }
    }
}

struct Func {
    name: String,
    hash: u32,
    start: usize,
    end: usize,
    jump_first: usize,
    jump_count: usize,
}

#[derive(Clone, Copy)]
struct Inst {
    arg0: usize,
    argc: usize,
    ret: u16,
    op: u16,
}

pub struct Script {
    pub path: String,
    funcs: Vec<Func>,
    jumps: Vec<(u32, usize)>,
    insts: Vec<Inst>,
    args: Vec<(u32, i32)>,
    strings: Vec<u8>,
}

impl Script {
    pub fn parse(path: &str, d: &[u8]) -> Result<Script> {
        if d.get(0..4) != Some(b"XQ32".as_slice()) {
            return Err(format!("{path}: não é XQ32"));
        }
        let h = |i: usize| u16le(d, 4 + 2 * i).map(|v| v as usize);
        let nfunc = h(0)?;
        let block =
            |words: usize| l5::decompress(d.get(words * 4..).ok_or("XQ32: offset inválido")?);
        let (f, j, i, a, s) = (
            block(h(1)?)?,
            block(h(2)?)?,
            block(h(4)?)?,
            block(h(6)?)?,
            block(h(9)?)?,
        );
        let (nj, ni, na) = (h(3)?, h(5)?, h(7)?);
        let strings = s;
        let name = |off: u32| -> String {
            let o = off as usize;
            let tail = strings.get(o..).unwrap_or(&[]);
            let end = tail.iter().position(|&b| b == 0).unwrap_or(tail.len());
            String::from_utf8_lossy(&tail[..end]).into_owned()
        };
        let funcs = (0..nfunc)
            .map(|k| {
                let o = 24 * k;
                Ok(Func {
                    name: name(u32le(&f, o)?),
                    hash: u32le(&f, o + 4)?,
                    start: u16le(&f, o + 8)? as usize,
                    end: u16le(&f, o + 10)? as usize,
                    jump_first: u16le(&f, o + 12)? as usize,
                    jump_count: u16le(&f, o + 14)? as usize,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let jumps = (0..nj)
            .map(|k| Ok((u32le(&j, 12 * k + 4)?, u32le(&j, 12 * k + 8)? as usize)))
            .collect::<Result<Vec<_>>>()?;
        let insts = (0..ni)
            .map(|k| {
                let o = 12 * k;
                Ok(Inst {
                    arg0: u16le(&i, o)? as usize,
                    argc: u16le(&i, o + 2)? as usize,
                    ret: u16le(&i, o + 4)?,
                    op: u16le(&i, o + 6)?,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let args = (0..na)
            .map(|k| Ok((u32le(&a, 8 * k)?, u32le(&a, 8 * k + 4)? as i32)))
            .collect::<Result<Vec<_>>>()?;
        Ok(Script {
            path: path.to_string(),
            funcs,
            jumps,
            insts,
            args,
            strings,
        })
    }

    fn string(&self, off: i32) -> Rc<str> {
        let tail = self.strings.get(off as usize..).unwrap_or(&[]);
        let end = tail.iter().position(|&b| b == 0).unwrap_or(tail.len());
        // Sem perda: cada byte vira um caractere latin-1 (nomes Shift-JIS de clipes/objetos comparam por bytes).
        Rc::from(latin1(&tail[..end]).as_str())
    }
}

/// O que um opcode nativo pede ao interpretador.
pub enum Native {
    /// Termina a instrução com este valor.
    Done(Value),
    /// Ainda não terminou: a instrução é repetida no próximo quadro (esperas do motor).
    Wait,
    /// Termina e cede o quadro.
    #[allow(dead_code)]
    Yield(Value),
}

/// Contexto de um opcode nativo.
#[allow(dead_code)] // script/thread: para hosts que precisem saber a origem
pub struct Call<'a> {
    pub op: u16,
    pub args: &'a [Value],
    pub script: &'a str,
    pub thread: usize,
}

pub trait Host {
    fn native(&mut self, call: Call) -> Native;
    /// Substitui uma função de script inteira (pelo hash) por um valor — para trechos que dependem de
    /// subsistemas ainda não reconstruídos (ex.: janelas de mensagem).
    fn intercept(&mut self, _function: u32, _args: &[Value], _thread: usize) -> Option<Native> {
        None
    }
}

struct Frame {
    script: usize,
    func: usize,
    pc: usize,
    vars: HashMap<u16, Value>,
    /// Variável do chamador que recebe o retorno.
    ret_to: Option<u16>,
}

struct Thread {
    frames: Vec<Frame>,
    name: String,
    ctx: Vec<Value>,
}

pub struct Vm {
    pub scripts: Vec<Script>,
    globals: Vec<HashMap<u16, Value>>,
    scene_args: Vec<Value>,
    threads: Vec<Option<Thread>>,
    /// Contexto herdado pelas corrotinas criadas pelo script (`op2211`).
    current_ctx: Vec<Value>,
    /// Funções por hash, em todos os scripts carregados (o próprio script tem prioridade).
    index: HashMap<u32, (usize, usize)>,
    pub warned: std::collections::HashSet<u16>,
    pub trace: bool,
}

const MAX_STEPS_PER_FRAME: usize = 200_000;
const BUSY_WAIT_JUMPS: usize = 256;

impl Vm {
    /// `scripts[0]` = script da cena; os demais = bibliotecas (`prog_common`, `prog_menu`...).
    pub fn new(scripts: Vec<Script>, scene_args: Vec<Value>) -> Vm {
        let mut index = HashMap::new();
        for (si, s) in scripts.iter().enumerate().rev() {
            for (fi, f) in s.funcs.iter().enumerate() {
                index.insert(f.hash, (si, fi));
            }
        }
        Vm {
            globals: scripts.iter().map(|_| HashMap::new()).collect(),
            scripts,
            scene_args,
            threads: Vec::new(),
            current_ctx: Vec::new(),
            index,
            warned: Default::default(),
            trace: false,
        }
    }

    /// Começa uma corrotina na função `hash` com os argumentos em `$3000..`.
    pub fn spawn(&mut self, hash: u32, args: &[Value]) -> Option<usize> {
        let ctx = self.current_ctx.clone();
        self.spawn_ctx(hash, args, ctx)
    }

    pub fn spawn_ctx(&mut self, hash: u32, args: &[Value], ctx: Vec<Value>) -> Option<usize> {
        let &(script, func) = self.index.get(&hash)?;
        let mut vars = HashMap::new();
        for (k, v) in args.iter().enumerate() {
            vars.insert(3000 + k as u16, v.clone());
        }
        let f = &self.scripts[script].funcs[func];
        let thread = Thread {
            ctx,
            name: f.name.clone(),
            frames: vec![Frame {
                script,
                func,
                pc: f.start,
                vars,
                ret_to: None,
            }],
        };
        let slot = self.threads.iter().position(Option::is_none);
        Some(match slot {
            Some(i) => {
                self.threads[i] = Some(thread);
                i
            }
            None => {
                self.threads.push(Some(thread));
                self.threads.len() - 1
            }
        })
    }

    /// Ponto de entrada da cena: a função com o mesmo nome do arquivo (ex. `TitleMenu`), senão a primeira.
    pub fn start_scene(&mut self) -> bool {
        let s = &self.scripts[0];
        let stem = s
            .path
            .rsplit('/')
            .next()
            .unwrap_or("")
            .trim_end_matches(".xq");
        let base = stem.split('_').next().unwrap_or(stem);
        let func = s
            .funcs
            .iter()
            .find(|f| f.name == stem || f.name == base)
            .or_else(|| s.funcs.iter().min_by_key(|f| f.start));
        let Some(h) = func.map(|f| f.hash) else {
            return false;
        };
        let ctx = self.scene_args.clone();
        self.spawn_ctx(h, &[], ctx).is_some()
    }

    /// Global `$var` (≥ 4000) de um script carregado.
    pub fn global(&self, script: usize, var: u16) -> Value {
        self.globals
            .get(script)
            .and_then(|g| g.get(&var))
            .cloned()
            .unwrap_or(Value::Int(0))
    }

    pub fn alive(&self) -> usize {
        self.threads.iter().flatten().count()
    }

    #[cfg(test)]
    pub fn thread_names(&self) -> Vec<String> {
        self.threads
            .iter()
            .flatten()
            .map(|t| {
                let at = t.frames.iter().map(|f| {
                    let s = &self.scripts[f.script];
                    format!("{}:{}", s.funcs[f.func].name, f.pc)
                });
                format!("{} [{}]", t.name, at.collect::<Vec<_>>().join(" > "))
            })
            .collect()
    }

    /// Cria uma corrotina a partir de um nome/hash de função vindo do host (callbacks de botões).
    pub fn spawn_value(&mut self, f: &Value, args: &[Value]) -> Option<usize> {
        self.spawn(f.key(), args)
    }

    /// Um quadro (1/60 s): cada corrotina roda até ceder.
    pub fn frame(&mut self, host: &mut dyn Host) -> Result<()> {
        let n = self.threads.len();
        for t in 0..n {
            self.run_thread(t, host)?;
        }
        Ok(())
    }

    fn read(&self, ctx: &[Value], frame: &Frame, var: i32) -> Value {
        let v = var as u16;
        match v {
            0..=999 => ctx.get(v as usize).cloned(),
            4000.. => self.globals[frame.script].get(&v).cloned(),
            _ => frame.vars.get(&v).cloned(),
        }
        .unwrap_or(Value::Int(0))
    }

    fn write(&mut self, t: usize, var: u16, value: Value) {
        let Some(th) = self.threads[t].as_mut() else {
            return;
        };
        let f = th.frames.last_mut().expect("quadro");
        let script = f.script;
        match var {
            0..=999 => {
                let i = var as usize;
                if th.ctx.len() <= i {
                    th.ctx.resize(i + 1, Value::Int(0));
                }
                th.ctx[i] = value;
            }
            4000.. => {
                self.globals[script].insert(var, value);
            }
            _ => {
                if let Some(f) = th.frames.last_mut() {
                    f.vars.insert(var, value);
                }
            }
        }
    }

    fn run_thread(&mut self, t: usize, host: &mut dyn Host) -> Result<()> {
        let mut back_jumps = 0usize;
        'step: for _ in 0..MAX_STEPS_PER_FRAME {
            let Some(th) = self.threads[t].as_ref() else {
                return Ok(());
            };
            let Some(fr) = th.frames.last() else {
                self.threads[t] = None;
                return Ok(());
            };
            let script = &self.scripts[fr.script];
            let func = &script.funcs[fr.func];
            if fr.pc >= func.end {
                self.ret(t, Value::Int(0));
                continue;
            }
            let inst = script.insts[fr.pc];
            let args: Vec<Value> = (0..inst.argc)
                .map(|k| {
                    let (ty, v) = script.args[inst.arg0 + k];
                    match ty {
                        1 => Value::Int(v),
                        2 => Value::Hash(v as u32),
                        3 => Value::Float(f32::from_bits(v as u32)),
                        4 => self.read(&th.ctx, fr, v),
                        24 => Value::Str(script.string(v)),
                        _ => Value::Int(v),
                    }
                })
                .collect();
            if self.trace {
                eprintln!(
                    "[xq] {}:{} {} op{} {:?}",
                    th.name,
                    fr.pc,
                    func.name,
                    inst.op,
                    args.iter().map(Value::text).collect::<Vec<_>>()
                );
            }
            let label = |hash: u32| -> Option<usize> {
                script.jumps[func.jump_first..func.jump_first + func.jump_count]
                    .iter()
                    .find(|(h, _)| *h == hash)
                    .map(|&(_, i)| i)
            };
            let a = |k: usize| args.get(k).cloned().unwrap_or(Value::Int(0));
            let num2 = |f: fn(f32, f32) -> f32, g: fn(i32, i32) -> i32| -> Value {
                match (a(0), a(1)) {
                    (Value::Float(x), y) => Value::Float(f(x, y.float())),
                    (x, Value::Float(y)) => Value::Float(f(x.float(), y)),
                    (x, y) => Value::Int(g(x.int(), y.int())),
                }
            };
            let cmp = |f: fn(f32, f32) -> bool| -> Value {
                let (x, y) = (a(0), a(1));
                let r = match (&x, &y) {
                    (Value::Str(p), Value::Str(q)) => f(p.cmp(q) as i32 as f32, 0.0),
                    _ => f(x.float(), y.float()),
                };
                Value::Int(r as i32)
            };
            let mut next = fr.pc + 1;
            let result: Option<Value> = match inst.op {
                10 => {
                    self.set_pc(t, next);
                    return Ok(());
                }
                11 => {
                    self.ret(t, a(0));
                    continue;
                }
                20 => 'call: {
                    match host.intercept(a(0).key(), &args[1.min(args.len())..], t) {
                        Some(Native::Done(v)) => break 'call Some(v),
                        Some(Native::Wait) => return Ok(()),
                        Some(Native::Yield(v)) => {
                            self.write(t, inst.ret, v);
                            self.set_pc(t, next);
                            return Ok(());
                        }
                        None => {}
                    }
                    let h = a(0).key();
                    match self.index.get(&h).copied() {
                        Some((si, fi)) => {
                            self.set_pc(t, next);
                            let mut vars = HashMap::new();
                            for (k, v) in args.iter().skip(1).enumerate() {
                                vars.insert(3000 + k as u16, v.clone());
                            }
                            let start = self.scripts[si].funcs[fi].start;
                            let th = self.threads[t].as_mut().expect("thread");
                            th.frames.push(Frame {
                                script: si,
                                func: fi,
                                pc: start,
                                vars,
                                ret_to: Some(inst.ret),
                            });
                            continue 'step;
                        }
                        None => {
                            if self.warned.insert(20) || self.trace {
                                eprintln!(
                                    "[xq] função #{h:08x} não encontrada (chamada em {})",
                                    func.name
                                );
                            }
                            Some(Value::Int(0))
                        }
                    }
                }
                30 | 31 | 33 => {
                    let take = match inst.op {
                        30 => a(1).truthy(),
                        33 => !a(1).truthy(),
                        _ => true,
                    };
                    if take {
                        next = label(a(0).key()).ok_or_else(|| {
                            format!(
                                "{}: rótulo {:08x} ausente em {}",
                                script.path,
                                a(0).key(),
                                func.name
                            )
                        })?;
                        // HIPÓTESE: laços de espera sem `yield` (ex. `while $4003 == -1`, esperando outra
                        // corrotina) cedem o quadro. Laços curtos (for) continuam no mesmo quadro.
                        if next <= fr.pc {
                            back_jumps += 1;
                            if back_jumps > BUSY_WAIT_JUMPS {
                                self.set_pc(t, next);
                                return Ok(());
                            }
                        }
                    }
                    None
                }
                // op100 com 3 argumentos = grava elemento de vetor: destino[i + j·largura] = v
                100 if args.len() >= 2 => {
                    if let Value::Array(arr) = self.read(&th.ctx, fr, inst.ret as i32) {
                        let mut arr = arr.borrow_mut();
                        let w = arr.0.max(1);
                        let idx = a(1).int().max(0) as usize + a(2).int().max(0) as usize * w;
                        if idx < arr.1.len() {
                            arr.1[idx] = a(0);
                        }
                        None
                    } else {
                        Some(a(0))
                    }
                }
                100 => Some(a(0)),
                530 => {
                    let (w, h) = (a(0).int().max(1) as usize, a(1).int().max(1) as usize);
                    Some(Value::Array(Rc::new(std::cell::RefCell::new((
                        w,
                        vec![Value::Int(0); w * h],
                    )))))
                }
                531 => Some(match a(0) {
                    Value::Array(arr) => {
                        let arr = arr.borrow();
                        let idx =
                            a(1).int().max(0) as usize + a(2).int().max(0) as usize * arr.0.max(1);
                        arr.1.get(idx).cloned().unwrap_or(Value::Int(0))
                    }
                    _ => Value::Int(0),
                }),
                120 => Some(Value::Int(!a(0).truthy() as i32)),
                // 121 = E, 122 = OU (o catálogo dizia o contrário; a leitura certa é a única que dá sentido a
                // `IsMessageNext` = op1000 OU op1103(0) OU ... e a `LoadMenu_Select`: estado == 0 OU == 1 → sai).
                121 => Some(Value::Int((a(0).truthy() && a(1).truthy()) as i32)),
                122 => Some(Value::Int((a(0).truthy() || a(1).truthy()) as i32)),
                130 => Some(Value::Int(match (a(0), a(1)) {
                    (Value::Str(x), Value::Str(y)) => (x == y) as i32,
                    (x, y) => {
                        (x.float() == y.float()
                            || x.key() == y.key() && !matches!(x, Value::Int(_) | Value::Float(_)))
                            as i32
                    }
                })),
                131 => Some(Value::Int(match (a(0), a(1)) {
                    (Value::Str(x), Value::Str(y)) => (x != y) as i32,
                    (x, y) => (x.float() != y.float()) as i32,
                })),
                // [H] ordem eq, ne, ge, le, gt, lt: é a que fecha os `for` dos scripts (slots `i <= 3` com 133,
                // teclas `i < $4004` com 135) e o contador de ociosidade do título (`> 1800` com 134).
                132 => Some(cmp(|x, y| x >= y)),
                133 => Some(cmp(|x, y| x <= y)),
                134 => Some(cmp(|x, y| x > y)),
                135 => Some(cmp(|x, y| x < y)),
                150 => Some(match (a(0), a(1)) {
                    (Value::Str(x), y) => Value::Str(Rc::from(format!("{x}{}", y.text()).as_str())),
                    _ => num2(|x, y| x + y, i32::wrapping_add),
                }),
                151 => Some(num2(|x, y| x - y, i32::wrapping_sub)),
                152 => Some(num2(|x, y| x * y, i32::wrapping_mul)),
                153 => Some(num2(
                    |x, y| if y == 0.0 { 0.0 } else { x / y },
                    |x, y| if y == 0 { 0 } else { x / y },
                )),
                154 => Some(Value::Int(match a(1).int() {
                    0 => 0,
                    y => a(0).int() % y,
                })),
                160 => Some(Value::Int(a(0).int() & a(1).int())),
                161 => Some(Value::Int(a(0).int() | a(1).int())),
                240 => Some(Value::Int(
                    self.read(&th.ctx, fr, inst.ret as i32).int() + 1,
                )),
                241 => Some(Value::Int(
                    self.read(&th.ctx, fr, inst.ret as i32).int() - 1,
                )),
                500 => Some(Value::Int(0)),
                501 => Some(Value::Str(Rc::from(sprintf(&args).as_str()))),
                522 => Some(a(0)),
                // relógio do jogo = relógio do PC (`clock.rs`)
                14041 => Some(Value::Int(crate::clock::query(a(0).int()))),
                14040 => {
                    crate::clock::set_game_time(a(0).int(), a(1).int());
                    Some(Value::Int(1))
                }
                // flags de progresso (`flags.rs`): 14000(chave, v = 1) grava, 14001(chave) lê o bit
                14000 => {
                    let v = if args.len() > 1 { a(1).int() } else { 1 };
                    crate::flags::set(a(0).key() as i32, v);
                    // `time_lock` também congela o relógio do jogo (`clock.rs`)
                    if a(0).key() == crate::assets::crc32(b"time_lock") {
                        crate::clock::set_lock(v != 0);
                    }
                    Some(Value::Int(1))
                }
                14001 => Some(Value::Int(crate::flags::get(a(0).key() as i32))),
                // [H] gravar/ler genéricos por tipo de flag (o tipo vem de `flag_config`, o código não foi lido):
                // 14003(chave, v) e 14005/14011(chave, v) gravam; 14004/14006/14012(chave) leem
                14003 | 14005 | 14011 => {
                    crate::flags::set(a(0).key() as i32, a(1).int());
                    Some(Value::Int(1))
                }
                14004 | 14006 | 14012 => Some(Value::Int(crate::flags::get(a(0).key() as i32))),
                2211 => {
                    self.current_ctx = th.ctx.clone();
                    let h = a(0).key();
                    let spawned = self.spawn(h, &args[1.min(args.len())..]);
                    if spawned.is_none() && self.warned.insert(2211) {
                        eprintln!("[xq] op2211: função {} não encontrada", a(0).text());
                    }
                    Some(Value::Int(spawned.map_or(-1, |i| i as i32)))
                }
                op => {
                    let path = self.scripts[fr.script].path.clone();
                    match host.native(Call {
                        op,
                        args: &args,
                        script: &path,
                        thread: t,
                    }) {
                        Native::Done(v) => Some(v),
                        Native::Wait => return Ok(()),
                        Native::Yield(v) => {
                            self.write(t, inst.ret, v);
                            self.set_pc(t, next);
                            return Ok(());
                        }
                    }
                }
            };
            if self.threads[t].is_none() {
                return Ok(()); // o host matou a corrotina
            }
            if let Some(v) = result {
                self.write(t, inst.ret, v);
            }
            self.set_pc(t, next);
        }
        let at = self.threads[t]
            .as_ref()
            .and_then(|th| th.frames.last())
            .map(|f| {
                let s = &self.scripts[f.script];
                format!("{} {}:{}", s.path, s.funcs[f.func].name, f.pc)
            })
            .unwrap_or_default();
        Err(format!(
            "corrotina {t} passou de {MAX_STEPS_PER_FRAME} passos num quadro (laço sem yield?) em {at}"
        ))
    }

    fn set_pc(&mut self, t: usize, pc: usize) {
        if let Some(f) = self.threads[t].as_mut().and_then(|th| th.frames.last_mut()) {
            f.pc = pc;
        }
    }

    fn ret(&mut self, t: usize, v: Value) {
        let Some(th) = self.threads[t].as_mut() else {
            return;
        };
        let done = th.frames.pop().expect("quadro");
        if th.frames.is_empty() {
            self.threads[t] = None;
            return;
        }
        if let Some(var) = done.ret_to {
            self.write(t, var, v);
        }
    }
}

/// Bytes → String sem perda (byte n = caractere U+00nn).
pub fn latin1(b: &[u8]) -> String {
    b.iter().map(|&c| c as char).collect()
}

/// Inverso de `latin1` (caracteres acima de U+00FF viram UTF-8 — nomes vindos do código Rust).
pub fn raw_bytes(s: &str) -> Vec<u8> {
    if s.chars().all(|c| (c as u32) < 256) {
        s.chars().map(|c| c as u8).collect()
    } else {
        s.as_bytes().to_vec()
    }
}

/// `op501`: printf mínimo (%d %s %f %x, com largura/zeros).
fn sprintf(args: &[Value]) -> String {
    let Some(fmt) = args.first().map(Value::text) else {
        return String::new();
    };
    let mut out = String::new();
    let mut rest = args.iter().skip(1);
    let mut chars = fmt.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        let mut spec = String::new();
        while let Some(&d) = chars.peek() {
            spec.push(d);
            chars.next();
            if d.is_ascii_alphabetic() || d == '%' {
                break;
            }
        }
        let conv = spec.chars().last().unwrap_or('d');
        let width: usize = spec
            .trim_end_matches(|c: char| c.is_ascii_alphabetic())
            .trim_start_matches('0')
            .parse()
            .unwrap_or(0);
        let zero = spec.starts_with('0');
        let s = match conv {
            '%' => "%".to_string(),
            'd' | 'i' | 'u' => rest.next().map_or(0, Value::int).to_string(),
            'x' => format!("{:x}", rest.next().map_or(0, Value::int)),
            'f' => format!("{}", rest.next().map_or(0.0, Value::float)),
            _ => rest.next().map_or(String::new(), Value::text),
        };
        if s.len() < width {
            let pad = if zero { '0' } else { ' ' };
            out.extend(std::iter::repeat_n(pad, width - s.len()));
        }
        out.push_str(&s);
    }
    out
}

/// Carrega o script de cena + bibliotecas comuns.
pub fn load_scene(fa: &crate::assets::arc0::Archive, path: &str) -> Result<Vec<Script>> {
    // toda cena passa por aqui: garante a tabela de flags (idempotente)
    crate::flags::init(fa);
    let mut v = vec![Script::parse(path, &fa.read(path)?)?];
    for lib in LIBRARIES {
        v.push(Script::parse(lib, &fa.read(lib)?)?);
    }
    Ok(v)
}

pub const LIBRARIES: [&str; 2] = ["seq/prog_menu_001e.xq", "seq/prog_common_001e.xq"];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sprintf_basic() {
        let s = sprintf(&[
            Value::Str("pass_%d/%02d %s".into()),
            Value::Int(3),
            Value::Int(7),
            Value::Str("x".into()),
        ]);
        assert_eq!(s, "pass_3/07 x");
    }

    struct Logger {
        ops: std::collections::BTreeMap<u16, usize>,
        events: Vec<String>,
    }

    impl Host for Logger {
        fn native(&mut self, c: Call) -> Native {
            *self.ops.entry(c.op).or_default() += 1;
            if matches!(c.op, 2104 | 14800 | 14806) {
                self.events.push(format!(
                    "op{} {:?}",
                    c.op,
                    c.args.iter().map(Value::text).collect::<Vec<_>>()
                ));
            }
            Native::Done(Value::Int(0))
        }
    }

    /// Roda o TitleMenu.xq real sem janela: tem de tocar a música do título e ficar vivo esperando entrada.
    #[test]
    fn title_script_runs() {
        let Some(romfs) = crate::romfs::locate(None) else {
            return;
        };
        let Ok(fa) = crate::assets::arc0::Archive::open(&romfs.join("yw1_a.fa")) else {
            return;
        };
        let scripts = load_scene(&fa, "seq/menu/TitleMenu.xq").unwrap();
        let mut vm = Vm::new(scripts, vec![Value::Int(2010)]);
        assert!(vm.start_scene());
        let mut host = Logger {
            ops: Default::default(),
            events: vec![],
        };
        for _ in 0..300 {
            vm.frame(&mut host).unwrap();
        }
        println!("eventos: {:?}", host.events);
        println!("opcodes nativos: {:?}", host.ops);
        println!("corrotinas: {:?}", vm.thread_names());
        assert!(host.events.iter().any(|e| e.contains("M01_MAIN_THEME")));
        assert!(vm.alive() > 0);
    }
}

#[cfg(test)]
mod survey {
    use super::*;

    struct Count(std::collections::BTreeMap<String, usize>, Vec<String>);
    impl Host for Count {
        fn native(&mut self, c: Call) -> Native {
            let key = if c.op == 2110 {
                format!("2110.{}", c.args.first().map_or(0, Value::int))
            } else {
                c.op.to_string()
            };
            *self.0.entry(key).or_default() += 1;
            if matches!(c.op, 2104 | 3502 | 3402 | 14800 | 5002) {
                self.1.push(format!(
                    "op{} {:?}",
                    c.op,
                    c.args.iter().map(Value::text).collect::<Vec<_>>()
                ));
            }
            Native::Done(Value::Int(0))
        }
    }

    /// Levantamento: `cargo test --release -- --ignored --nocapture survey_menus`.
    #[test]
    #[ignore]
    fn survey_menus() {
        let romfs = crate::romfs::locate(None).unwrap();
        let fa = crate::assets::arc0::Archive::open(&romfs.join("yw1_a.fa")).unwrap();
        for (path, args) in [
            (
                "seq/menu/SaveLoadMenu_001e.xq",
                vec![
                    Value::Int(2111),
                    Value::Int(-1),
                    Value::Int(0),
                    Value::Int(1),
                ],
            ),
            ("seq/menu/CharaMakeMenu_001e.xq", vec![Value::Int(2060)]),
            ("seq/menu/NameMenu_001e.xq", vec![Value::Int(2070)]),
        ] {
            let mut vm = Vm::new(load_scene(&fa, path).unwrap(), args);
            println!("== {path}: início {}", vm.start_scene());
            let mut h = Count(Default::default(), vec![]);
            for _ in 0..600 {
                if let Err(e) = vm.frame(&mut h) {
                    println!("ERRO {e}");
                    break;
                }
            }
            println!("ops {:?}", h.0);
            for e in h.1.iter().take(40) {
                println!("  {e}");
            }
            println!("corrotinas {:?}", vm.thread_names());
        }
    }
}
