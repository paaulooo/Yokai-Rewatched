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
    uvec4 depth;     // x = máximo do formato de profundidade (0xFFFF ou 0xFFFFFF)
} u;

layout(set = 0, binding = 1) uniform sampler2D t0;
layout(set = 0, binding = 2) uniform sampler2D t1;
layout(set = 0, binding = 3) uniform sampler2D t2;

// flags (misc.w)
const uint F_LIGHTING = 1u;
const uint F_TEX2_TC1 = 2u;
const uint F_SCISSOR_EXCLUDE = 4u;
const uint F_TEX0 = 16u;

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

ivec4 sampleTex(sampler2D s, uvec4 info, vec2 uv) {
    int w = int(info.x & 0xffffu);
    int h = int(info.x >> 16);
    if (w == 0 || h == 0) return ivec4(0, 0, 0, 255);
    float fs = uv.x * float(w);
    float ft = uv.y * float(h);
    if ((info.y & 0x100u) == 0u) return fetch(s, info, int(floor(fs)), int(floor(ft)));
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
    return clamp(ivec4(a * (1.0 - fy) + b * fy + 0.5), 0, 255);
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

void main() {
    uint flags = u.misc.w;

    // scissor de exclusão (modo 1): descarta dentro do retângulo
    if ((flags & F_SCISSOR_EXCLUDE) != 0u) {
        uvec2 p = uvec2(gl_FragCoord.xy);
        if (p.x >= u.scissor.x && p.x <= u.scissor.z && p.y >= u.scissor.y && p.y <= u.scissor.w) discard;
    }

    // cor primária: |cor| saturada por vértice já veio da CPU; aqui só interpola e trunca como `as u8`
    ivec4 primary = clamp(ivec4(min(v_color, vec4(1.0)) * 255.0), 0, 255);

    ivec4 texs[3] = ivec4[3](ivec4(0, 0, 0, 255), ivec4(0, 0, 0, 255), ivec4(0, 0, 0, 255));
    if ((flags & (F_TEX0 << 0)) != 0u) texs[0] = sampleTex(t0, u.tex[0], v_tc0);
    if ((flags & (F_TEX0 << 1)) != 0u) texs[1] = sampleTex(t1, u.tex[1], v_tc1);
    if ((flags & (F_TEX0 << 2)) != 0u) texs[2] = sampleTex(t2, u.tex[2], (flags & F_TEX2_TC1) != 0u ? v_tc1 : v_tc2);

    // iluminação por fragmento não implementada [H] (mesma convenção do soft)
    bool lit = (flags & F_LIGHTING) != 0u;
    ivec4 fragP = lit ? ivec4(255) : ivec4(0);
    ivec4 fragS = ivec4(0);

    ivec4 outc = primary;
    ivec4 buf = ivec4(0);
    ivec4 nextBuf = unpackRgba(u.misc.x);
    uint bufUpd = u.misc.y;

    for (int si = 0; si < 6; si++) {
        uvec4 st = u.tev[si];
        ivec4 konst = unpackRgba(st.w);
        ivec4 srcs[16];
        for (int k = 0; k < 16; k++) srcs[k] = ivec4(0);
        srcs[0] = primary;
        srcs[1] = fragP;
        srcs[2] = fragS;
        srcs[3] = texs[0];
        srcs[4] = texs[1];
        srcs[5] = texs[2];
        srcs[13] = buf;
        srcs[14] = konst;
        srcs[15] = outc;

        ivec3 rin[3];
        int ain[3];
        for (int k = 0; k < 3; k++) {
            rin[k] = opRgb((st.y >> (4 * k)) & 0xfu, srcs[(st.x >> (4 * k)) & 0xfu]);
            ain[k] = opA((st.y >> (12 + 4 * k)) & 7u, srcs[(st.x >> (16 + 4 * k)) & 0xfu]);
        }
        uint combRgb = st.z & 0xfu;
        uint combA = (st.z >> 16) & 0xfu;
        ivec3 rgb;
        if (combRgb == 6u || combRgb == 7u) {
            int d = 0;
            for (int k = 0; k < 3; k++) d += divTrunc((rin[0][k] * 2 - 255) * (rin[1][k] * 2 - 255) + 128, 256);
            rgb = ivec3(clamp(d, 0, 255));
        } else {
            for (int k = 0; k < 3; k++) rgb[k] = comb(combRgb, rin[0][k], rin[1][k], rin[2][k]);
        }
        int al = combRgb == 7u ? rgb.r : comb(combA, ain[0], ain[1], ain[2]);
        uint sc = u.scale[si / 4][si % 4];
        int sr = 1 << min(sc & 3u, 2u);
        int sa = 1 << min((sc >> 16) & 3u, 2u);
        rgb = min(rgb * sr, ivec3(255));
        al = min(al * sa, 255);
        outc = ivec4(rgb, al);
        buf = nextBuf;
        if (si < 4) {
            if (((bufUpd >> si) & 1u) != 0u) nextBuf.rgb = outc.rgb;
            if (((bufUpd >> (4 + si)) & 1u) != 0u) nextBuf.a = outc.a;
        }
    }

    // alpha test: z = ligado | função << 4 | referência << 8
    uint at = u.misc.z;
    if ((at & 1u) != 0u && !testFn((at >> 4) & 7u, outc.a, int((at >> 8) & 0xffu))) discard;

    // profundidade: inteiro do formato (truncado como no soft), guardado exato como zi·2⁻²⁴
    float z = clamp(gl_FragCoord.z, 0.0, 1.0);
    uint zi = uint(z * float(u.depth.x));
    gl_FragDepth = float(zi) * (1.0 / 16777216.0);

    o_color = vec4(outc) / 255.0;
}
