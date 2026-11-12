#version 450
// "Ubershader" de fragmento do PICA: reproduz, com aritmética inteira, o `raster.rs::fragment` do backend soft —
// amostragem das 3 unidades de textura (wrap/borda/bilinear feitos aqui com texelFetch, como `texture::sample`),
// TEV genérico de 6 estágios + buffer de combinação, alpha test, scissor de exclusão e profundidade quantizada
// (inteiro do formato do PICA, guardado como zi·2⁻²⁴ no D32F para as comparações serem exatas).
// Blending, logic op, máscaras, depth/stencil test ficam no estado fixo do pipeline.

layout(location = 0) in vec4 v_color;
layout(location = 1) in vec2 v_tc0;
layout(location = 2) in vec2 v_tc1;
layout(location = 3) in vec2 v_tc2;

layout(location = 0) out vec4 o_color;

// Mesmo layout de render::DrawUniforms (std140: cada uvec4 = 16 bytes).
layout(set = 0, binding = 0, std140) uniform Draw {
    uvec4 tev[6];    // por estágio: x = fontes, y = operandos, z = combinadores, w = constante RGBA (r no byte 0)
    uvec4 scale[2];  // estágio i: scale[i/4][i%4] = escala_rgb | escala_a << 16
    uvec4 misc;      // x = cor inicial do buffer, y = máscara de atualização do buffer, z = alpha test, w = flags
    uvec4 scissor;   // x0, y0, x1, y1 (inclusivos) do scissor de exclusão
    uvec4 tex[3];    // x = largura | altura << 16, y = wrap_s | wrap_t << 4 | linear << 8, z = cor da borda
    uvec4 depth;     // x = máximo do formato de profundidade (0xFFFF ou 0xFFFFFF), y = zscale (bits f32),
                     // z = zoffset (bits f32), w = origem da profundidade (DEPTH_*) | DEPTH_WBUF
} u;

// Fator da resolução interna do alvo em que este draw está sendo gravado (1 = nativo; N na "sombra" N×, vulkan.rs
// hires): coordenadas de tela do PICA = gl_FragCoord / n.
layout(push_constant) uniform PC { uint n; } pc;

layout(set = 0, binding = 1) uniform sampler2D t0;
layout(set = 0, binding = 2) uniform sampler2D t1;
layout(set = 0, binding = 3) uniform sampler2D t2;

// Especialização (vulkan.rs `FsKey`): com SPEC = true o estado de controle do draw vem destas constantes e não do
// uniform — o driver dobra o TEV, os modos de textura e as flags ao criar o pipeline (o mesmo código do ubershader,
// então o resultado é o mesmo por construção). Valores (cores, constantes do TEV, tamanhos, referência do alpha test,
// zscale/zoffset, scissor) continuam no uniform. SPEC = false = ubershader (o padrão, sem especialização).
layout(constant_id = 0) const bool SPEC = false;
layout(constant_id = 1) const uint SP_NST = 6u;      // estágios executados (os de depois são passagem direta)
layout(constant_id = 2) const uint SP_FLAGS = 0u;    // misc.w
layout(constant_id = 3) const uint SP_BUFUPD = 0u;   // misc.y
layout(constant_id = 4) const uint SP_ATEST = 0u;    // misc.z & 0xff (ligado | função << 4)
layout(constant_id = 5) const uint SP_DEPTH = 0u;    // depth.w
layout(constant_id = 6) const uint SP_TEX0 = 0u;     // tex[i].y (wrap_s | wrap_t << 4 | linear << 8)
layout(constant_id = 7) const uint SP_TEX1 = 0u;
layout(constant_id = 8) const uint SP_TEX2 = 0u;
// por estágio i: fontes (10 + 4i), operandos (11 + 4i), combinadores (12 + 4i), escala (13 + 4i)
layout(constant_id = 10) const uint SP_S0 = 0u;
layout(constant_id = 11) const uint SP_O0 = 0u;
layout(constant_id = 12) const uint SP_C0 = 0u;
layout(constant_id = 13) const uint SP_K0 = 0u;
layout(constant_id = 14) const uint SP_S1 = 0u;
layout(constant_id = 15) const uint SP_O1 = 0u;
layout(constant_id = 16) const uint SP_C1 = 0u;
layout(constant_id = 17) const uint SP_K1 = 0u;
layout(constant_id = 18) const uint SP_S2 = 0u;
layout(constant_id = 19) const uint SP_O2 = 0u;
layout(constant_id = 20) const uint SP_C2 = 0u;
layout(constant_id = 21) const uint SP_K2 = 0u;
layout(constant_id = 22) const uint SP_S3 = 0u;
layout(constant_id = 23) const uint SP_O3 = 0u;
layout(constant_id = 24) const uint SP_C3 = 0u;
layout(constant_id = 25) const uint SP_K3 = 0u;
layout(constant_id = 26) const uint SP_S4 = 0u;
layout(constant_id = 27) const uint SP_O4 = 0u;
layout(constant_id = 28) const uint SP_C4 = 0u;
layout(constant_id = 29) const uint SP_K4 = 0u;
layout(constant_id = 30) const uint SP_S5 = 0u;
layout(constant_id = 31) const uint SP_O5 = 0u;
layout(constant_id = 32) const uint SP_C5 = 0u;
layout(constant_id = 33) const uint SP_K5 = 0u;

uint drawFlags() { return SPEC ? SP_FLAGS : u.misc.w; }
uvec4 texInfo(int i) {
    uvec4 t = u.tex[i];
    if (SPEC) t.y = i == 0 ? SP_TEX0 : i == 1 ? SP_TEX1 : SP_TEX2;
    return t;
}

// flags (misc.w)
const uint F_LIGHTING = 1u;
const uint F_TEX2_TC1 = 2u;
const uint F_SCISSOR_EXCLUDE = 4u;
const uint F_TEX0 = 16u;
const uint F_ANISO_SHIFT = 8u; // bits 8–11: n → até 2^n amostras por textura (0 = desligado)

// origem da profundidade (depth.w)
const uint DEPTH_FRAGCOORD = 0u; // vértices da CPU: gl_FragCoord.z já é a profundidade do PICA
const uint DEPTH_ZNDC = 1u;      // vértices da GPU: gl_FragCoord.z = −z/w do PICA → zoffset − zscale·gl_FragCoord.z
// bit: w-buffer (0x6D bit 0 = 0). O PICA guarda (z/w·zscale + zoffset)·w com z/w interpolado na tela e w com
// correção de perspectiva (Citra: "z·scale + w·offset = (z/w·scale + offset)·w"; Zakuro raster.frag faz
// `depth /= gl_FragCoord.w`) → o valor de cima dividido por gl_FragCoord.w (= 1/w interpolado na tela). Vale nos dois
// caminhos, inclusive nos vértices criados pelo recorte do hardware (w deles é o certo).
const uint DEPTH_WBUF = 4u;

ivec4 unpackRgba(uint v) {
    return ivec4(int(v & 0xffu), int((v >> 8) & 0xffu), int((v >> 16) & 0xffu), int(v >> 24));
}

// Coordenada inteira com o modo de repetição (texture.rs::wrap); ok = false → fora (borda).
int wrapc(uint mode, int v, int n, inout bool ok) {
    uint m = mode & 3u;
    if (m == 0u) return clamp(v, 0, n - 1);
    if (m == 1u) {
        if (v < 0 || v >= n) ok = false;
        return clamp(v, 0, n - 1);
    }
    // resto euclidiano sem depender do sinal do % do GLSL
    if (m == 2u) {
        int r = v - n * int(floor(float(v) / float(n)));
        return clamp(r, 0, n - 1);
    }
    int n2 = 2 * n;
    int r2 = v - n2 * int(floor(float(v) / float(n2)));
    r2 = clamp(r2, 0, n2 - 1);
    return r2 >= n ? n2 - 1 - r2 : r2;
}

ivec4 fetch(sampler2D s, uvec4 info, int x, int y) {
    int w = int(info.x & 0xffffu);
    int h = int(info.x >> 16);
    bool ok = true;
    int sx = wrapc(info.y & 7u, x, w, ok);
    int sy = wrapc((info.y >> 4) & 7u, y, h, ok);
    if (!ok) return unpackRgba(info.z);
    // imagem decodificada na CPU com a linha 0 = t 0 (texture::fetch já aplica a inversão da memória)
    return ivec4(round(texelFetch(s, ivec2(sx, sy), 0) * 255.0));
}

// Uma amostra bilinear (texture::sample) em coordenadas de texel (fs, ft), SEM arredondar: o filtro anisotrópico soma
// várias e só arredonda no fim.
vec4 bilinear(sampler2D s, uvec4 info, float fs, float ft) {
    float x0 = floor(fs - 0.5);
    float y0 = floor(ft - 0.5);
    float fx = fs - 0.5 - x0;
    float fy = ft - 0.5 - y0;
    int ix = int(x0);
    int iy = int(y0);
    vec4 c00 = vec4(fetch(s, info, ix, iy));
    vec4 c10 = vec4(fetch(s, info, ix + 1, iy));
    vec4 c01 = vec4(fetch(s, info, ix, iy + 1));
    vec4 c11 = vec4(fetch(s, info, ix + 1, iy + 1));
    vec4 a = c00 * (1.0 - fx) + c10 * fx;
    vec4 b = c01 * (1.0 - fx) + c11 * fx;
    return a * (1.0 - fy) + b * fy;
}

ivec4 sampleTex(sampler2D s, uvec4 info, vec2 uv) {
    int w = int(info.x & 0xffffu);
    int h = int(info.x >> 16);
    if (w == 0 || h == 0) return ivec4(0, 0, 0, 255);
    float fs = uv.x * float(w);
    float ft = uv.y * float(h);
    if ((info.y & 0x100u) == 0u) return fetch(s, info, int(floor(fs)), int(floor(ft)));
    // Anisotrópico (opção, fora do hardware real; o PICA só tem 1 amostra bilinear): as texturas do jogo não têm
    // mipmaps, então quando a pegada do pixel cobre mais de 1 texel (superfície inclinada/distante) uma amostra só
    // serrilha e cintila. Aqui a pegada (derivadas de tela da coordenada em texels) é coberta por uma grade de
    // amostras bilineares — nx ao longo do eixo maior, ny ao longo do menor, no máximo 2^n no total — e a média
    // substitui a amostra única. Pegada ≤ 1 texel (ou opção desligada) = exatamente o caminho de uma amostra.
    // Fica em fluxo uniforme (flags vêm do uniform), então dFdx/dFdy valem.
    uint an = (drawFlags() >> F_ANISO_SHIFT) & 15u;
    if (an != 0u) {
        vec2 dx = dFdx(vec2(fs, ft));
        vec2 dy = dFdy(vec2(fs, ft));
        float lx = length(dx);
        float ly = length(dy);
        bool xmaior = lx >= ly;
        vec2 va = xmaior ? dx : dy;
        vec2 vb = xmaior ? dy : dx;
        float la = xmaior ? lx : ly;
        float lb = xmaior ? ly : lx;
        if (la < 1.0e6) {
            int maxn = 1 << int(an);
            int nx = clamp(int(ceil(la - 1.0e-3)), 1, maxn);
            int ny = clamp(int(ceil(lb - 1.0e-3)), 1, max(maxn / nx, 1));
            if (nx * ny > 1) {
                vec4 sum = vec4(0.0);
                for (int j = 0; j < ny; j++) {
                    for (int i = 0; i < nx; i++) {
                        vec2 off = ((float(i) + 0.5) / float(nx) - 0.5) * va + ((float(j) + 0.5) / float(ny) - 0.5) * vb;
                        sum += bilinear(s, info, fs + off.x, ft + off.y);
                    }
                }
                return clamp(ivec4(sum / float(nx * ny) + 0.5), 0, 255);
            }
        }
    }
    return clamp(ivec4(bilinear(s, info, fs, ft) + 0.5), 0, 255);
}

ivec3 opRgb(uint op, ivec4 s) {
    switch (op) {
        case 0u: return s.rgb;
        case 1u: return 255 - s.rgb;
        case 2u: return ivec3(s.a);
        case 3u: return ivec3(255 - s.a);
        case 4u: return ivec3(s.r);
        case 5u: return ivec3(255 - s.r);
        case 8u: return ivec3(s.g);
        case 9u: return ivec3(255 - s.g);
        case 12u: return ivec3(s.b);
        case 13u: return ivec3(255 - s.b);
        default: return s.rgb;
    }
}

int opA(uint op, ivec4 s) {
    switch (op) {
        case 0u: return s.a;
        case 1u: return 255 - s.a;
        case 2u: return s.r;
        case 3u: return 255 - s.r;
        case 4u: return s.g;
        case 5u: return 255 - s.g;
        case 6u: return s.b;
        default: return 255 - s.b;
    }
}

int comb(uint op, int a, int b, int c) {
    switch (op) {
        case 0u: return a;
        case 1u: return a * b / 255;
        case 2u: return min(a + b, 255);
        case 3u: return clamp(a + b - 128, 0, 255);
        case 4u: return (a * c + b * (255 - c)) / 255;
        case 5u: return max(a - b, 0);
        case 8u: return min((a * b + 255 * c) / 255, 255);
        case 9u: return (min(a + b, 255) * c) / 255;
        default: return a;
    }
}

// divisão inteira truncando para zero (como o Rust), independente do sinal
int divTrunc(int n, int d) {
    int q = abs(n) / d;
    return n < 0 ? -q : q;
}

bool testFn(uint f, int a, int b) {
    switch (f) {
        case 0u: return false;
        case 1u: return true;
        case 2u: return a == b;
        case 3u: return a != b;
        case 4u: return a < b;
        case 5u: return a <= b;
        case 6u: return a > b;
        default: return a >= b;
    }
}

// Fonte do TEV (0 primária, 1/2 iluminação, 3–5 texturas, 13 buffer, 14 constante, 15 anterior; o resto é 0). Um
// switch em vez de um vetor local `ivec4[16]` indexado em tempo de execução: o vetor ia para a memória de rascunho da
// GPU (scratch) em cada pixel e em cada estágio — o custo dominante da sombra N× (medido com timestamps da GPU).
ivec4 tevSrc(uint i, ivec4 primary, ivec4 fragP, ivec4 fragS, ivec4 t0v, ivec4 t1v, ivec4 t2v, ivec4 buf, ivec4 konst, ivec4 outc) {
    switch (i) {
        case 0u: return primary;
        case 1u: return fragP;
        case 2u: return fragS;
        case 3u: return t0v;
        case 4u: return t1v;
        case 5u: return t2v;
        case 13u: return buf;
        case 14u: return konst;
        case 15u: return outc;
        default: return ivec4(0);
    }
}

// Estado do TEV entre estágios (variáveis do módulo; depois de inlinar viram registradores).
ivec4 g_primary, g_fragP, g_fragS, g_tx0, g_tx1, g_tx2, g_outc, g_buf, g_nextBuf;

// Um estágio do TEV: fontes, operandos, combinadores e escala do estágio `si` (constantes com SPEC, uniform sem).
void tevStage(int si, uint src, uint opw, uint cmb, uint sc, uint bufUpd) {
    ivec4 konst = unpackRgba(u.tev[si].w);
    ivec3 rin[3];
    int ain[3];
    for (int k = 0; k < 3; k++) {
        rin[k] = opRgb((opw >> (4 * k)) & 0xfu, tevSrc((src >> (4 * k)) & 0xfu, g_primary, g_fragP, g_fragS, g_tx0, g_tx1, g_tx2, g_buf, konst, g_outc));
        ain[k] = opA((opw >> (12 + 4 * k)) & 7u, tevSrc((src >> (16 + 4 * k)) & 0xfu, g_primary, g_fragP, g_fragS, g_tx0, g_tx1, g_tx2, g_buf, konst, g_outc));
    }
    uint combRgb = cmb & 0xfu;
    uint combA = (cmb >> 16) & 0xfu;
    ivec3 rgb;
    if (combRgb == 6u || combRgb == 7u) {
        int d = 0;
        for (int k = 0; k < 3; k++) d += divTrunc((rin[0][k] * 2 - 255) * (rin[1][k] * 2 - 255) + 128, 256);
        rgb = ivec3(clamp(d, 0, 255));
    } else {
        for (int k = 0; k < 3; k++) rgb[k] = comb(combRgb, rin[0][k], rin[1][k], rin[2][k]);
    }
    int al = combRgb == 7u ? rgb.r : comb(combA, ain[0], ain[1], ain[2]);
    int sr = 1 << min(sc & 3u, 2u);
    int sa = 1 << min((sc >> 16) & 3u, 2u);
    rgb = min(rgb * sr, ivec3(255));
    al = min(al * sa, 255);
    g_outc = ivec4(rgb, al);
    g_buf = g_nextBuf;
    if (si < 4) {
        if (((bufUpd >> si) & 1u) != 0u) g_nextBuf.rgb = g_outc.rgb;
        if (((bufUpd >> (4 + si)) & 1u) != 0u) g_nextBuf.a = g_outc.a;
    }
}

void main() {
    uint flags = drawFlags();

    // scissor de exclusão (modo 1): descarta dentro do retângulo
    if ((flags & F_SCISSOR_EXCLUDE) != 0u) {
        uvec2 p = uvec2(gl_FragCoord.xy) / pc.n;
        if (p.x >= u.scissor.x && p.x <= u.scissor.z && p.y >= u.scissor.y && p.y <= u.scissor.w) discard;
    }

    // cor primária: |cor| saturada por vértice já veio da CPU; aqui só interpola e trunca como `as u8`
    g_primary = clamp(ivec4(min(v_color, vec4(1.0)) * 255.0), 0, 255);

    g_tx0 = ivec4(0, 0, 0, 255);
    g_tx1 = ivec4(0, 0, 0, 255);
    g_tx2 = ivec4(0, 0, 0, 255);
    if ((flags & (F_TEX0 << 0)) != 0u) g_tx0 = sampleTex(t0, texInfo(0), v_tc0);
    if ((flags & (F_TEX0 << 1)) != 0u) g_tx1 = sampleTex(t1, texInfo(1), v_tc1);
    if ((flags & (F_TEX0 << 2)) != 0u) g_tx2 = sampleTex(t2, texInfo(2), (flags & F_TEX2_TC1) != 0u ? v_tc1 : v_tc2);

    // iluminação por fragmento não implementada [H] (mesma convenção do soft)
    bool lit = (flags & F_LIGHTING) != 0u;
    g_fragP = lit ? ivec4(255) : ivec4(0);
    g_fragS = ivec4(0);

    g_outc = g_primary;
    g_buf = ivec4(0);
    g_nextBuf = unpackRgba(u.misc.x);

    if (SPEC) {
        // desenrolado: cada chamada recebe constantes e o driver dobra o estágio inteiro
        if (SP_NST > 0u) tevStage(0, SP_S0, SP_O0, SP_C0, SP_K0, SP_BUFUPD);
        if (SP_NST > 1u) tevStage(1, SP_S1, SP_O1, SP_C1, SP_K1, SP_BUFUPD);
        if (SP_NST > 2u) tevStage(2, SP_S2, SP_O2, SP_C2, SP_K2, SP_BUFUPD);
        if (SP_NST > 3u) tevStage(3, SP_S3, SP_O3, SP_C3, SP_K3, SP_BUFUPD);
        if (SP_NST > 4u) tevStage(4, SP_S4, SP_O4, SP_C4, SP_K4, SP_BUFUPD);
        if (SP_NST > 5u) tevStage(5, SP_S5, SP_O5, SP_C5, SP_K5, SP_BUFUPD);
    } else {
        // estágios finais de passagem direta (anterior, sem operando, Replace, escala 1) não mudam a cor nem são
        // lidos por ninguém: o backend manda quantos executar em tex[0].w (render::uniforms, `tev_active`)
        int nst = int(min(u.tex[0].w, 6u));
        for (int si = 0; si < nst; si++) tevStage(si, u.tev[si].x, u.tev[si].y, u.tev[si].z, u.scale[si / 4][si % 4], u.misc.y);
    }
    ivec4 outc = g_outc;

    // alpha test: z = ligado | função << 4 | referência << 8
    uint at = SPEC ? (SP_ATEST | (u.misc.z & 0xff00u)) : u.misc.z;
    if ((at & 1u) != 0u && !testFn((at >> 4) & 7u, outc.a, int((at >> 8) & 0xffu))) discard;

#ifndef FAST_Z
    // profundidade: inteiro do formato (truncado como no soft), guardado exato como zi·2⁻²⁴
    float zraw = gl_FragCoord.z;
    uint dmode = SPEC ? SP_DEPTH : u.depth.w;
    if ((dmode & 3u) == DEPTH_ZNDC) zraw = uintBitsToFloat(u.depth.z) - uintBitsToFloat(u.depth.y) * gl_FragCoord.z;
    if ((dmode & DEPTH_WBUF) != 0u) zraw /= gl_FragCoord.w;
    float z = clamp(zraw, 0.0, 1.0);
    uint zi = uint(z * float(u.depth.x));
    gl_FragDepth = float(zi) * (1.0 / 16777216.0);
#endif
    // FAST_Z (variante `pica_frag_fastz.spv`, F4: sombra N× e intermediários da interpolação): nenhuma escrita de
    // gl_FragDepth — a profundidade é a interpolada pelo rasterizador, com a faixa do viewport ajustada pelo backend,
    // e o módulo não declara DepthReplacing (o teste antecipado de profundidade volta a valer). O `discard` do alpha
    // test e do scissor continua.

    o_color = vec4(outc) / 255.0;
}
