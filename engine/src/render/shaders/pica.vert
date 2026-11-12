#version 450
// Vértices já sombreados, recortados e projetados na CPU (render::prepare): a posição chega pronta em coordenadas de
// recorte do Vulkan (x, y com o viewport do PICA já aplicado e arredondado a 1/16 de pixel; z = profundidade do
// z-buffer·w — com w-buffer o pica.frag ainda divide por gl_FragCoord.w).
// Este estágio só repassa os atributos; o Vulkan faz a interpolação com correção de perspectiva pelo w.

layout(location = 0) in vec4 a_pos;
layout(location = 1) in vec4 a_color;
layout(location = 2) in vec2 a_tc0;
layout(location = 3) in vec2 a_tc1;
layout(location = 4) in vec2 a_tc2;

layout(location = 0) out vec4 v_color;
layout(location = 1) out vec2 v_tc0;
layout(location = 2) out vec2 v_tc1;
layout(location = 3) out vec2 v_tc2;

void main() {
    gl_Position = a_pos;
    v_color = a_color;
    v_tc0 = a_tc0;
    v_tc1 = a_tc1;
    v_tc2 = a_tc2;
}
