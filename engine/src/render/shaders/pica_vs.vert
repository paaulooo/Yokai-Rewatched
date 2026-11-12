#version 450
// Shader de vértice do PICA200 INTERPRETADO NA GPU ("ubershader" de vértice) — mesma semântica do interpretador de
// CPU `pica/shader.rs` (opcodes, descritores com swizzle/negação, 0 × inf = 0, a0/aL, cc, CALL/IF/LOOP/BREAK/JMP,
// pilha de 16 quadros que descarta o mais antigo, limite de 100 000 passos).
//
// Ideia emprestada do Zakuro (MIT, crates/zakuro-gpu/shaders/shade.vert): ler os arranjos de vértices crus da
// memória guest de um storage buffer, mapear as saídas por "semânticas" e deixar o recorte para o hardware — o PICA
// recorta z em −w..0, que vira o 0..w do Vulkan com gl_Position.z = −z. Diferenças deliberadas para o Zakuro, para
// bater com o nosso backend soft: registradores de saída começam em (0, 0, 0, 1), temporários em 0, uniform fora
// das 96 constantes lê (1, 1, 1, 1), LITP mantém z e usa cc = (x > 0, w > 0), encaixe 12.4 arredonda "metade para
// longe do zero" (o `f32::round` do soft) e semântica sem registrador vale 0 (posição w = 1).
//
// Entradas (set 1): parâmetros do draw (uniforms do PICA + mapeamentos), programa (4096 palavras + 128 descritores,
// em cache por conteúdo no backend) e os bytes dos arranjos de vértices usados pelo draw.
// Saídas: as mesmas que o pica.vert entrega ao pica.frag (cor saturada |c| ≤ 1, tc0/tc1/tc2) e o plano de recorte
// do usuário (gl_ClipDistance). A profundidade (com ou sem w-buffer) sai de gl_FragCoord no pica.frag.
//
// Shaders AOT (programas do jogo traduzidos pelo gerador, recomp/src/pica_glsl.rs): são ESTE arquivo com o trecho
// entre os marcadores @@AOT-BEGIN / @@AOT-END trocado pelo programa traduzido (run, loadInputs, outComp). Todo o
// resto (interface, leitura dos atributos, auxiliares aritméticos, pilha, saídas, encaixe 12.4) é compartilhado, então
// a semântica é a mesma por construção. Mudou este arquivo → regere (cd recomp && cargo run --release): o motor recusa
// shaders AOT gerados a partir de outra versão (hash do arquivo no registro).

layout(std430, set = 1, binding = 0) readonly buffer Shading {
    vec4 cf[96];        // uniforms float c0..c95
    ivec4 ci[4];        // uniforms int i0..i3: x = contagem, y = início de aL, z = passo (com sinal)
    uvec4 misc;         // x = uniforms bool (16 bits), y = ponto de entrada, z = flags (F_*)
    uvec4 sem[6];       // semântica k (0..23): sem[k/4][k%4] = registrador de saída·4 + componente, ou NONE
    vec4 vp;            // viewport do PICA: meia largura, meia altura, x, y (pixels)
    vec4 zmap;          // x = zscale, y = zoffset (não lido aqui: a profundidade sai do pica.frag)
    vec4 clipc;         // coeficientes do plano de recorte do usuário
    uvec4 attr[16];     // por registrador de entrada: x = byte do vértice 0, y = passo, z = tipo | componentes << 8
    vec4 defs[16];      // valor do registrador onde o atributo não dá componente (atributo fixo, padrão 0,0,0,1)
} s;

layout(std430, set = 1, binding = 1) readonly buffer Prog {
    uint code[4096];
    uint desc[128];
} p;

layout(std430, set = 1, binding = 2) readonly buffer Verts {
    uint vb[];
} v;

layout(location = 0) out vec4 v_color;
layout(location = 1) out vec2 v_tc0;
layout(location = 2) out vec2 v_tc1;
layout(location = 3) out vec2 v_tc2;

out gl_PerVertex {
    vec4 gl_Position;
    float gl_ClipDistance[1];
};

const uint NONE = 0xffffffffu;
const uint F_WBUFFER = 1u;
const uint F_USER_CLIP = 2u;
const int STACK = 16;
const uint MAX_STEPS = 100000u;

vec4 vin[16];
vec4 tmp[16];
vec4 vout[16];
ivec2 a0;
int al;
bvec2 cc;

// pilha de CALL/IF/LOOP (shader.rs::Stack)
uint fr_final[STACK];
uint fr_ret[STACK];
uint fr_rep[STACK];
int fr_incr[STACK];
uint fr_loop[STACK];
bool fr_looped[STACK];
int nfr = 0;

// ---------------------------------------------------------------------------------------------------------------
// atributos crus (pica/mod.rs::load_vertex)

uint byteAt(uint at) {
    return (v.vb[at >> 2u] >> ((at & 3u) * 8u)) & 0xffu;
}

// palavra little-endian a partir de um byte qualquer (pode atravessar a palavra seguinte)
uint wordAt(uint at) {
    uint sh = (at & 3u) * 8u;
    uint lo = v.vb[at >> 2u];
    if (sh == 0u) return lo;
    return (lo >> sh) | (v.vb[(at >> 2u) + 1u] << (32u - sh));
}

float component(uint at, uint ty) {
    if (ty == 0u) {
        int b = int(byteAt(at));
        return float(b >= 128 ? b - 256 : b);
    }
    if (ty == 1u) return float(byteAt(at));
    if (ty == 2u) {
        int h = int(wordAt(at) & 0xffffu);
        return float(h >= 32768 ? h - 65536 : h);
    }
    return uintBitsToFloat(wordAt(at));
}

vec4 inputRegister(uint r, uint index) {
    uvec4 a = s.attr[r];
    vec4 val = s.defs[r];
    uint n = (a.z >> 8u) & 0xffu;
    if (n == 0u) return val;
    uint ty = a.z & 0xffu;
    uint es = ty == 3u ? 4u : (ty == 2u ? 2u : 1u);
    uint at = a.x + index * a.y;
    for (uint c = 0u; c < 4u; c++) {
        if (c < n) val[c] = component(at + c * es, ty);
    }
    return val;
}

// ---------------------------------------------------------------------------------------------------------------
// interpretador (pica/shader.rs::Exec)

// PICA: 0 × qualquer coisa (inclusive ±inf) = 0, exceto NaN
float pmul(float a, float b) {
    if ((a == 0.0 && !isnan(b)) || (b == 0.0 && !isnan(a))) return 0.0;
    precise float r = a * b;
    return r;
}

vec4 pmul4(vec4 a, vec4 b) {
    return vec4(pmul(a.x, b.x), pmul(a.y, b.y), pmul(a.z, b.z), pmul(a.w, b.w));
}

// soma na mesma ordem do `(0..n).map(..).sum()` do Rust, sem fusão em FMA
float dot3p(vec4 a, vec4 b) {
    precise float d = pmul(a.x, b.x) + pmul(a.y, b.y);
    d = d + pmul(a.z, b.z);
    return d;
}

float dot4p(vec4 a, vec4 b) {
    precise float d = dot3p(a, b) + pmul(a.w, b.w);
    return d;
}

// `f32 as i32` do Rust: satura, NaN = 0
int toInt(float x) {
    if (isnan(x)) return 0;
    if (x >= 2147483647.0) return 2147483647;
    if (x <= -2147483648.0) return -2147483647 - 1;
    return int(x);
}

// uniform float com endereçamento relativo já somado: 7 bits; fora das 96 constantes lê (1, 1, 1, 1)
vec4 unif(int idx) {
    idx &= 0x7f;
    return idx < 96 ? s.cf[idx] : vec4(1.0);
}

int relOf(uint idx) {
    if (idx == 1u) return a0.x;
    if (idx == 2u) return a0.y;
    if (idx == 3u) return al;
    return 0;
}

// seletor de 8 bits: x nos bits 7–6, y 5–4, z 3–2, w 1–0
vec4 swz(vec4 x, uint sel, bool neg) {
    vec4 o = vec4(x[(sel >> 6u) & 3u], x[(sel >> 4u) & 3u], x[(sel >> 2u) & 3u], x[sel & 3u]);
    return neg ? -o : o;
}

bool cond(uint w) {
    bool refy = ((w >> 24u) & 1u) != 0u;
    bool refx = ((w >> 25u) & 1u) != 0u;
    bool x = cc.x == refx;
    bool y = cc.y == refy;
    uint m = (w >> 22u) & 3u;
    if (m == 0u) return x || y;
    if (m == 1u) return x && y;
    if (m == 2u) return x;
    return y;
}

bool boolU(uint w) {
    return ((s.misc.x >> ((w >> 22u) & 0xfu)) & 1u) != 0u;
}

// empilha; cheia: descarta o quadro mais antigo (como Stack::push)
uint push(uint off, uint num, uint ret, uint rep, int incr, bool looped) {
    if (nfr == STACK) {
        for (int i = 1; i < STACK; i++) {
            fr_final[i - 1] = fr_final[i];
            fr_ret[i - 1] = fr_ret[i];
            fr_rep[i - 1] = fr_rep[i];
            fr_incr[i - 1] = fr_incr[i];
            fr_loop[i - 1] = fr_loop[i];
            fr_looped[i - 1] = fr_looped[i];
        }
        nfr = STACK - 1;
    }
    fr_final[nfr] = off + num;
    fr_ret[nfr] = ret;
    fr_rep[nfr] = rep;
    fr_incr[nfr] = incr;
    fr_loop[nfr] = off;
    fr_looped[nfr] = looped;
    nfr++;
    return off;
}

// BREAK: tira até o laço mais interno (inclusive); devolve o retorno dele ou `fallback` se não há laço
uint brk(uint fallback) {
    for (int i = nfr - 1; i >= 0; i--) {
        if (fr_looped[i]) {
            nfr = i;
            return fr_ret[i];
        }
    }
    return fallback;
}

bool cmpOp(float a, float b, uint o) {
    if (o == 0u) return a == b;
    if (o == 1u) return a != b;
    if (o == 2u) return a < b;
    if (o == 3u) return a <= b;
    if (o == 4u) return a > b;
    if (o == 5u) return a >= b;
    return true;
}

// @@AOT-BEGIN — interpretador (trocado pelo programa traduzido nos shaders AOT)

// 7 bits: 0x00–0x0F v, 0x10–0x1F r, 0x20–0x7F c (com endereçamento relativo só nos uniforms)
vec4 src(uint r, int rel) {
    if (r < 0x10u) return vin[r];
    if (r < 0x20u) return tmp[r - 0x10u];
    return unif(int(r) - 0x20 + rel);
}

void wr(uint dest, uint mask, vec4 val) {
    if (dest >= 0x20u) return;
    for (uint k = 0u; k < 4u; k++) {
        if (((mask >> (3u - k)) & 1u) == 0u) continue;
        if (dest < 0x10u) vout[dest][k] = val[k];
        else tmp[dest - 0x10u][k] = val[k];
    }
}

// formato 1 / 1i / CMP
void arith(uint w, uint op) {
    uint d = p.desc[w & 0x7fu];
    bool inv = op >= 0x18u && op <= 0x1bu;
    uint idx = (w >> 19u) & 3u;
    vec4 r1, r2;
    if (inv) {
        r1 = src((w >> 14u) & 0x1fu, 0);
        r2 = src((w >> 7u) & 0x7fu, relOf(idx));
    } else {
        r1 = src((w >> 12u) & 0x7fu, relOf(idx));
        r2 = src((w >> 7u) & 0x1fu, 0);
    }
    vec4 s1 = swz(r1, (d >> 5u) & 0xffu, ((d >> 4u) & 1u) != 0u);
    vec4 s2 = swz(r2, (d >> 14u) & 0xffu, ((d >> 13u) & 1u) != 0u);
    uint mask = d & 0xfu;
    uint dest = (w >> 21u) & 0x1fu;
    precise vec4 r;
    switch (op) {
        case 0x00u: r = s1 + s2; break;
        case 0x01u: r = vec4(dot3p(s1, s2)); break;
        case 0x02u: r = vec4(dot4p(s1, s2)); break;
        case 0x03u:
        case 0x18u: {
            precise float t = dot3p(s1, s2) + s2.w;
            r = vec4(t);
            break;
        }
        case 0x04u:
        case 0x19u: r = vec4(1.0, pmul(s1.y, s2.y), s1.z, s2.w); break;
        case 0x05u: r = vec4(exp2(s1.x)); break;
        case 0x06u: r = vec4(log2(s1.x)); break;
        case 0x07u:
            // LITP (Citra): x = max(x, 0), y = clamp(y, ±127.9961), z = z, w = max(w, 0); cc = (x > 0, w > 0)
            r = vec4(max(s1.x, 0.0), clamp(s1.y, -127.9961, 127.9961), s1.z, max(s1.w, 0.0));
            cc = bvec2(s1.x > 0.0, s1.w > 0.0);
            break;
        case 0x08u: r = pmul4(s1, s2); break;
        case 0x09u:
        case 0x1au: r = vec4(greaterThanEqual(s1, s2)); break;
        case 0x0au:
        case 0x1bu: r = vec4(lessThan(s1, s2)); break;
        case 0x0bu: r = floor(s1); break;
        case 0x0cu:
            r = vec4(s1.x > s2.x ? s1.x : s2.x, s1.y > s2.y ? s1.y : s2.y, s1.z > s2.z ? s1.z : s2.z,
                     s1.w > s2.w ? s1.w : s2.w);
            break;
        case 0x0du:
            r = vec4(s1.x < s2.x ? s1.x : s2.x, s1.y < s2.y ? s1.y : s2.y, s1.z < s2.z ? s1.z : s2.z,
                     s1.w < s2.w ? s1.w : s2.w);
            break;
        case 0x0eu: r = vec4(1.0 / s1.x); break;
        case 0x0fu: r = vec4(1.0 / sqrt(s1.x)); break;
        case 0x12u:
            if ((mask & 8u) != 0u) a0.x = toInt(s1.x);
            if ((mask & 4u) != 0u) a0.y = toInt(s1.y);
            return;
        case 0x13u: r = s1; break;
        case 0x2eu:
        case 0x2fu:
            cc = bvec2(cmpOp(s1.x, s2.x, (w >> 24u) & 7u), cmpOp(s1.y, s2.y, (w >> 21u) & 7u));
            return;
        default: return;
    }
    wr(dest, mask, r);
}

// MADI (0x30–0x37) / MAD (0x38–0x3F)
void mad(uint w, uint op) {
    uint d = p.desc[w & 0x1fu];
    int rel = relOf((w >> 22u) & 3u);
    vec4 r1, r2, r3;
    r1 = src((w >> 17u) & 0x1fu, 0);
    if (op >= 0x38u) {
        r2 = src((w >> 10u) & 0x7fu, rel);
        r3 = src((w >> 5u) & 0x1fu, 0);
    } else {
        r2 = src((w >> 12u) & 0x1fu, 0);
        r3 = src((w >> 5u) & 0x7fu, rel);
    }
    vec4 s1 = swz(r1, (d >> 5u) & 0xffu, ((d >> 4u) & 1u) != 0u);
    vec4 s2 = swz(r2, (d >> 14u) & 0xffu, ((d >> 13u) & 1u) != 0u);
    vec4 s3 = swz(r3, (d >> 23u) & 0xffu, ((d >> 22u) & 1u) != 0u);
    precise vec4 r = pmul4(s1, s2) + s3;
    wr((w >> 24u) & 0x1fu, d & 0xfu, r);
}

void run() {
    uint pc = s.misc.y;
    uint steps = 0u;
    for (;;) {
        steps++;
        if (steps > MAX_STEPS || pc >= 4096u) return;
        if (nfr > 0 && pc == fr_final[nfr - 1]) {
            int t = nfr - 1;
            al += fr_incr[t];
            if (fr_rep[t] == 0u) {
                pc = fr_ret[t];
                nfr--;
            } else {
                fr_rep[t]--;
                pc = fr_loop[t];
            }
            continue;
        }
        uint w = p.code[pc];
        uint op = w >> 26u;
        uint next = pc + 1u;
        uint num = w & 0xffu;
        uint dst = (w >> 10u) & 0xfffu;
        if (op <= 0x1bu || op == 0x2eu || op == 0x2fu) {
            arith(w, op);
        } else if (op >= 0x30u) {
            mad(w, op);
        } else if (op == 0x20u) {
            next = brk(next);
        } else if (op == 0x22u) {
            return;
        } else if (op == 0x23u) {
            if (cond(w)) next = brk(next);
        } else if (op == 0x24u || op == 0x25u || op == 0x26u) {
            bool go = op == 0x24u ? true : (op == 0x25u ? cond(w) : boolU(w));
            if (go) next = push(dst, num, pc + 1u, 0u, 0, false);
        } else if (op == 0x27u || op == 0x28u) {
            bool c = op == 0x27u ? boolU(w) : cond(w);
            if (c) next = push(pc + 1u, dst - pc - 1u, dst + num, 0u, 0, false);
            else next = push(dst, num, dst + num, 0u, 0, false);
        } else if (op == 0x29u) {
            ivec4 iu = s.ci[(w >> 22u) & 3u];
            al = iu.y;
            next = push(pc + 1u, dst - pc, dst + 1u, uint(iu.x), iu.z, true);
        } else if (op == 0x2cu) {
            if (cond(w)) next = dst;
        } else if (op == 0x2du) {
            if (boolU(w) == ((w & 1u) == 0u)) next = dst;
        }
        pc = next;
    }
}

// registradores de entrada (todos; o AOT lê só os que o programa usa)
void loadInputs(uint index) {
    for (uint r = 0u; r < 16u; r++) vin[r] = inputRegister(r, index);
}

// componente x & 3 do registrador de saída x >> 2
float outComp(uint x) {
    return vout[x >> 2u][x & 3u];
}

// @@AOT-END

// ---------------------------------------------------------------------------------------------------------------
// saídas (pica/mod.rs::output_vertex + render::prepare)

float semv(uint k, float dflt) {
    uint x = s.sem[k >> 2u][k & 3u];
    return x == NONE ? dflt : outComp(x);
}

// `f32::round` do Rust (metade para longe do zero), exato
float roundAway(float x) {
    float t = trunc(x);
    return abs(x - t) >= 0.5 ? t + sign(x) : t;
}

void main() {
    uint index = uint(gl_VertexIndex);
    loadInputs(index);
    for (uint r = 0u; r < 16u; r++) {
        tmp[r] = vec4(0.0);
        vout[r] = vec4(0.0, 0.0, 0.0, 1.0);
    }
    a0 = ivec2(0);
    al = 0;
    cc = bvec2(false);
    run();

    vec4 pos = vec4(semv(0u, 0.0), semv(1u, 0.0), semv(2u, 0.0), semv(3u, 1.0));
    vec4 col = vec4(semv(8u, 0.0), semv(9u, 0.0), semv(10u, 0.0), semv(11u, 0.0));
    v_color = min(abs(col), vec4(1.0));
    v_tc0 = vec2(semv(12u, 0.0), semv(13u, 0.0));
    v_tc1 = vec2(semv(14u, 0.0), semv(15u, 0.0));
    v_tc2 = vec2(semv(22u, 0.0), semv(23u, 0.0));

    // O viewport do Vulkan é o do PICA: x_tela = (x/w + 1)·meia_largura + x0. Encaixa na grade de 1/16 de pixel
    // (12.4) aqui, com as contas do soft, e devolve em coordenadas de recorte; o GPU (≥ 8 bits de subpixel) cai na
    // mesma grade. Vértice com w ≤ 0 não é visível: fica sem encaixe e o recorte do hardware resolve.
    vec4 clip = pos;
    float w = pos.w;
    precise float iw = 1.0 / w;
    if (w > 0.0) {
        precise float sx = (pos.x * iw + 1.0) * s.vp.x + s.vp.z;
        precise float sy = (pos.y * iw + 1.0) * s.vp.y + s.vp.w;
        float x16 = roundAway(sx * 16.0) / 16.0;
        float y16 = roundAway(sy * 16.0) / 16.0;
        pos.x = ((x16 - s.vp.z) / s.vp.x - 1.0) * w;
        pos.y = ((y16 - s.vp.w) / s.vp.y - 1.0) * w;
    }
    // PICA: −w ≤ z ≤ 0 → Vulkan: 0 ≤ z ≤ w. A profundidade sai de gl_FragCoord no pica.frag: zoffset −
    // zscale·z_ndc, e com w-buffer isso dividido por gl_FragCoord.w (= ·w com correção de perspectiva, como o PICA).
    gl_Position = vec4(pos.x, pos.y, -pos.z, w);
    gl_ClipDistance[0] = (s.misc.z & F_USER_CLIP) != 0u ? dot(s.clipc, clip) : 1.0;
}
