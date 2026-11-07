"""Pipeline minimo em Vulkan: renderiza off-screen (sem swapchain/janela) UM draw real da
captura (data/capture/cap, draw 0 = quad de 4 vertices / 6 indices, o mais simples do quadro)
usando o SPIR-V JA COMPILADO de verdade a partir do vertex shader PICA real
(work/shaders/prog0_0x0000_0x001c.spv, gerado por tools/pica_shader_to_glsl.py +
glslangValidator). Objetivo deste passo: provar que a cadeia completa
"shader_code.bin real -> desmontador -> GLSL -> SPIR-V -> Vulkan -> rasterizacao" funciona,
nao ainda bater pixel-a-pixel com o jogo.

IMPORTANTE (nao e' "hipotese assumida como fato", e' uma limitacao documentada): os uniforms
c[] REAIS capturados para este draw (ver tools/pica_uniforms.py) produzem, matematicamente, um
quad degenerado (a matriz de mundo real c[8..10] ignora completamente v0.x - o primeiro canal da
posicao - entao as 4 posicoes colapsam no mesmo ponto). Isso indica que a correspondencia exata
"qual dos 7 programas de shader roda em qual draw" (via VSH_ENTRYPOINT) AINDA NAO esta validada
(o registrador que deveria conter o entry point leu um valor fora do intervalo do code mem,
0x7fff0000, nesta captura). Por isso aqui os uniforms sao substituidos por uma matriz ortografica
construida a mao (mundo=view=identidade, projecao = mapeamento linear 512x240 -> NDC), so' para
validar o lado Vulkan/SPIR-V. A posicao e os indices do vertice SAO os reais da captura.
Uso: python vk_render_min.py
"""
import ctypes, os, struct, sys
import numpy as np
import vulkan as vk
from PIL import Image

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from paths import CAP, WORK
from frame_sim import run, attrs, vertices

W, H = 512, 240
VERT_SPV = os.path.join(os.path.dirname(__file__), '..', 'work', 'shaders', 'prog0_0x0000_0x001c.spv')
FRAG_SRC = os.path.join(os.path.dirname(__file__), '..', 'work', 'shaders', 'min_test.frag')
FRAG_SPV = os.path.join(os.path.dirname(__file__), '..', 'work', 'shaders', 'min_test.frag.spv')
OUT_PNG = os.path.join(os.path.dirname(__file__), '..', 'work', 'shaders', 'vk_render_min.png')


def load_draw0():
    C = str(CAP) + os.sep
    frame = [C + 'list_%02d.bin' % i for i in range(1, 8)]
    D = run(frame)
    L, R = D[0]
    nv = R[0x228]
    idx, out = vertices(R, nv)
    pos = out[0].astype('<f4')  # (N,2) pixel space, real da captura
    return idx.astype('<u4'), pos


def find_memtype(mem_props, type_bits, flags):
    for i in range(mem_props.memoryTypeCount):
        if (type_bits & (1 << i)) and (mem_props.memoryTypes[i].propertyFlags & flags) == flags:
            return i
    raise RuntimeError('nenhum memory type compativel')


def alloc_buffer(device, mem_props, size, usage, mem_flags):
    buf = vk.vkCreateBuffer(device, vk.VkBufferCreateInfo(size=size, usage=usage,
                             sharingMode=vk.VK_SHARING_MODE_EXCLUSIVE), None)
    req = vk.vkGetBufferMemoryRequirements(device, buf)
    mt = find_memtype(mem_props, req.memoryTypeBits, mem_flags)
    mem = vk.vkAllocateMemory(device, vk.VkMemoryAllocateInfo(
        allocationSize=req.size, memoryTypeIndex=mt), None)
    vk.vkBindBufferMemory(device, buf, mem, 0)
    return buf, mem


def upload(device, mem, data_bytes):
    buf = vk.vkMapMemory(device, mem, 0, len(data_bytes), 0)
    buf[:] = data_bytes
    vk.vkUnmapMemory(device, mem)


def main():
    os.makedirs(os.path.dirname(VERT_SPV), exist_ok=True)
    if not os.path.exists(FRAG_SPV):
        with open(FRAG_SRC, 'w') as f:
            f.write("""#version 450
layout(location=1) in vec4 o1_out;
layout(location=0) out vec4 frag_color;
void main() { frag_color = o1_out; }
""")
        rc = os.system('"C:/msys64/ucrt64/bin/glslangValidator.exe" -S frag -V "%s" -o "%s"' % (
            FRAG_SRC.replace('\\', '/'), FRAG_SPV.replace('\\', '/')))
        if rc != 0:
            raise RuntimeError('falha ao compilar o fragment shader de teste')

    idx, pos = load_draw0()
    print('indices:', idx, 'posicoes reais (pixel):\n', pos)
    nverts = pos.shape[0]
    color_data = np.tile(np.array([1, 0, 0, 1], dtype='<f4'), (nverts, 1))  # vermelho, so p/ ver o quad
    v4_dummy = np.zeros((nverts, 2), dtype='<f4')
    v5_dummy = np.zeros((nverts, 2), dtype='<f4')

    app_info = vk.VkApplicationInfo(pApplicationName='yw1-vk-min', apiVersion=vk.VK_API_VERSION_1_0)
    instance = vk.vkCreateInstance(vk.VkInstanceCreateInfo(pApplicationInfo=app_info), None)
    phys = vk.vkEnumeratePhysicalDevices(instance)[0]
    for pd in vk.vkEnumeratePhysicalDevices(instance):
        if vk.vkGetPhysicalDeviceProperties(pd).deviceType == vk.VK_PHYSICAL_DEVICE_TYPE_DISCRETE_GPU:
            phys = pd
    mem_props = vk.vkGetPhysicalDeviceMemoryProperties(phys)
    qfams = vk.vkGetPhysicalDeviceQueueFamilyProperties(phys)
    gfx = next(i for i, q in enumerate(qfams) if q.queueFlags & vk.VK_QUEUE_GRAPHICS_BIT)
    device = vk.vkCreateDevice(phys, vk.VkDeviceCreateInfo(pQueueCreateInfos=[
        vk.VkDeviceQueueCreateInfo(queueFamilyIndex=gfx, queueCount=1, pQueuePriorities=[1.0])]), None)
    queue = vk.vkGetDeviceQueue(device, gfx, 0)
    print('GPU:', vk.vkGetPhysicalDeviceProperties(phys).deviceName)

    def make_vbuf(arr):
        data = arr.tobytes()
        buf, mem = alloc_buffer(device, mem_props, len(data), vk.VK_BUFFER_USAGE_VERTEX_BUFFER_BIT,
                                 vk.VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT | vk.VK_MEMORY_PROPERTY_HOST_COHERENT_BIT)
        upload(device, mem, data)
        return buf, mem

    vb_pos, _ = make_vbuf(pos)
    vb_color, _ = make_vbuf(color_data)
    vb_v4, _ = make_vbuf(v4_dummy)
    vb_v5, _ = make_vbuf(v5_dummy)
    ib_data = idx.tobytes()
    ibuf, ibuf_mem = alloc_buffer(device, mem_props, len(ib_data), vk.VK_BUFFER_USAGE_INDEX_BUFFER_BIT,
                                   vk.VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT | vk.VK_MEMORY_PROPERTY_HOST_COHERENT_BIT)
    upload(device, ibuf_mem, ib_data)

    # uniform buffer: matriz ortografica construida a mao (ver docstring) -- World=View=Identidade,
    # Projecao mapeia [0,W]x[0,H] em pixels para NDC [-1,1], Z=0, W=1 sempre (2D).
    c = [[0.0, 0.0, 0.0, 0.0] for _ in range(96)]
    c[0] = [2.0 / W, 0.0, 0.0, -1.0]
    c[1] = [0.0, 2.0 / H, 0.0, -1.0]
    c[2] = [0.0, 0.0, 0.0, 0.0]
    c[3] = [0.0, 0.0, 0.0, 1.0]
    c[4] = [1.0, 0.0, 0.0, 0.0]
    c[5] = [0.0, 1.0, 0.0, 0.0]
    c[6] = [0.0, 0.0, 1.0, 0.0]
    c[8] = [1.0, 0.0, 0.0, 0.0]
    c[9] = [0.0, 1.0, 0.0, 0.0]
    c[10] = [0.0, 0.0, 1.0, 0.0]
    c[11] = [1.0, 1.0, 1.0, 1.0]
    c[91] = [0.0, 1.0, 0.0, 0.0]
    c[92] = [0.0, 0.0, 1.0, 0.0]
    uniforms = struct.pack('<%df' % (96 * 4), *[x for row in c for x in row])
    ubuf, ubuf_mem = alloc_buffer(device, mem_props, len(uniforms), vk.VK_BUFFER_USAGE_UNIFORM_BUFFER_BIT,
                                   vk.VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT | vk.VK_MEMORY_PROPERTY_HOST_COHERENT_BIT)
    upload(device, ubuf_mem, uniforms)

    # imagem de render-target off-screen
    img = vk.vkCreateImage(device, vk.VkImageCreateInfo(
        imageType=vk.VK_IMAGE_TYPE_2D, format=vk.VK_FORMAT_R8G8B8A8_UNORM,
        extent=vk.VkExtent3D(width=W, height=H, depth=1), mipLevels=1, arrayLayers=1,
        samples=vk.VK_SAMPLE_COUNT_1_BIT, tiling=vk.VK_IMAGE_TILING_OPTIMAL,
        usage=vk.VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT | vk.VK_IMAGE_USAGE_TRANSFER_SRC_BIT,
        sharingMode=vk.VK_SHARING_MODE_EXCLUSIVE, initialLayout=vk.VK_IMAGE_LAYOUT_UNDEFINED), None)
    img_req = vk.vkGetImageMemoryRequirements(device, img)
    img_mem = vk.vkAllocateMemory(device, vk.VkMemoryAllocateInfo(
        allocationSize=img_req.size,
        memoryTypeIndex=find_memtype(mem_props, img_req.memoryTypeBits, vk.VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT)), None)
    vk.vkBindImageMemory(device, img, img_mem, 0)
    view = vk.vkCreateImageView(device, vk.VkImageViewCreateInfo(
        image=img, viewType=vk.VK_IMAGE_VIEW_TYPE_2D, format=vk.VK_FORMAT_R8G8B8A8_UNORM,
        subresourceRange=vk.VkImageSubresourceRange(aspectMask=vk.VK_IMAGE_ASPECT_COLOR_BIT,
                                                      baseMipLevel=0, levelCount=1, baseArrayLayer=0, layerCount=1)), None)

    attachment = vk.VkAttachmentDescription(
        format=vk.VK_FORMAT_R8G8B8A8_UNORM, samples=vk.VK_SAMPLE_COUNT_1_BIT,
        loadOp=vk.VK_ATTACHMENT_LOAD_OP_CLEAR, storeOp=vk.VK_ATTACHMENT_STORE_OP_STORE,
        stencilLoadOp=vk.VK_ATTACHMENT_LOAD_OP_DONT_CARE, stencilStoreOp=vk.VK_ATTACHMENT_STORE_OP_DONT_CARE,
        initialLayout=vk.VK_IMAGE_LAYOUT_UNDEFINED, finalLayout=vk.VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL)
    color_ref = vk.VkAttachmentReference(attachment=0, layout=vk.VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL)
    subpass = vk.VkSubpassDescription(pipelineBindPoint=vk.VK_PIPELINE_BIND_POINT_GRAPHICS,
                                       pColorAttachments=[color_ref])
    renderpass = vk.vkCreateRenderPass(device, vk.VkRenderPassCreateInfo(
        pAttachments=[attachment], pSubpasses=[subpass]), None)
    fb = vk.vkCreateFramebuffer(device, vk.VkFramebufferCreateInfo(
        renderPass=renderpass, pAttachments=[view], width=W, height=H, layers=1), None)

    def load_spv(path):
        data = open(path, 'rb').read()
        return vk.vkCreateShaderModule(device, vk.VkShaderModuleCreateInfo(
            codeSize=len(data), pCode=data), None)
    vs = load_spv(VERT_SPV)
    fs = load_spv(FRAG_SPV)

    dsl = vk.vkCreateDescriptorSetLayout(device, vk.VkDescriptorSetLayoutCreateInfo(pBindings=[
        vk.VkDescriptorSetLayoutBinding(binding=0, descriptorType=vk.VK_DESCRIPTOR_TYPE_UNIFORM_BUFFER,
                                         descriptorCount=1, stageFlags=vk.VK_SHADER_STAGE_VERTEX_BIT)]), None)
    pool = vk.vkCreateDescriptorPool(device, vk.VkDescriptorPoolCreateInfo(
        maxSets=1, pPoolSizes=[vk.VkDescriptorPoolSize(type=vk.VK_DESCRIPTOR_TYPE_UNIFORM_BUFFER, descriptorCount=1)]), None)
    dset = vk.vkAllocateDescriptorSets(device, vk.VkDescriptorSetAllocateInfo(
        descriptorPool=pool, pSetLayouts=[dsl]))[0]
    vk.vkUpdateDescriptorSets(device, 1, [vk.VkWriteDescriptorSet(
        dstSet=dset, dstBinding=0, descriptorCount=1, descriptorType=vk.VK_DESCRIPTOR_TYPE_UNIFORM_BUFFER,
        pBufferInfo=[vk.VkDescriptorBufferInfo(buffer=ubuf, offset=0, range=len(uniforms))])], 0, [])

    pipe_layout = vk.vkCreatePipelineLayout(device, vk.VkPipelineLayoutCreateInfo(pSetLayouts=[dsl]), None)

    bindings = [
        vk.VkVertexInputBindingDescription(binding=0, stride=8, inputRate=vk.VK_VERTEX_INPUT_RATE_VERTEX),
        vk.VkVertexInputBindingDescription(binding=1, stride=16, inputRate=vk.VK_VERTEX_INPUT_RATE_VERTEX),
        vk.VkVertexInputBindingDescription(binding=2, stride=8, inputRate=vk.VK_VERTEX_INPUT_RATE_VERTEX),
        vk.VkVertexInputBindingDescription(binding=3, stride=8, inputRate=vk.VK_VERTEX_INPUT_RATE_VERTEX),
    ]
    attrsdesc = [
        vk.VkVertexInputAttributeDescription(location=0, binding=0, format=vk.VK_FORMAT_R32G32_SFLOAT, offset=0),
        vk.VkVertexInputAttributeDescription(location=1, binding=1, format=vk.VK_FORMAT_R32G32B32A32_SFLOAT, offset=0),
        vk.VkVertexInputAttributeDescription(location=4, binding=2, format=vk.VK_FORMAT_R32G32_SFLOAT, offset=0),
        vk.VkVertexInputAttributeDescription(location=5, binding=3, format=vk.VK_FORMAT_R32G32_SFLOAT, offset=0),
    ]
    vertex_input = vk.VkPipelineVertexInputStateCreateInfo(pVertexBindingDescriptions=bindings,
                                                            pVertexAttributeDescriptions=attrsdesc)
    input_asm = vk.VkPipelineInputAssemblyStateCreateInfo(topology=vk.VK_PRIMITIVE_TOPOLOGY_TRIANGLE_LIST)
    viewport = vk.VkViewport(x=0.0, y=0.0, width=float(W), height=float(H), minDepth=0.0, maxDepth=1.0)
    scissor = vk.VkRect2D(offset=vk.VkOffset2D(x=0, y=0), extent=vk.VkExtent2D(width=W, height=H))
    viewport_state = vk.VkPipelineViewportStateCreateInfo(pViewports=[viewport], pScissors=[scissor])
    raster = vk.VkPipelineRasterizationStateCreateInfo(polygonMode=vk.VK_POLYGON_MODE_FILL,
                                                        cullMode=vk.VK_CULL_MODE_NONE, frontFace=vk.VK_FRONT_FACE_CLOCKWISE,
                                                        lineWidth=1.0)
    multisample = vk.VkPipelineMultisampleStateCreateInfo(rasterizationSamples=vk.VK_SAMPLE_COUNT_1_BIT)
    blend_attachment = vk.VkPipelineColorBlendAttachmentState(colorWriteMask=0xF, blendEnable=0)
    blend = vk.VkPipelineColorBlendStateCreateInfo(pAttachments=[blend_attachment])
    stages = [
        vk.VkPipelineShaderStageCreateInfo(stage=vk.VK_SHADER_STAGE_VERTEX_BIT, module=vs, pName='main'),
        vk.VkPipelineShaderStageCreateInfo(stage=vk.VK_SHADER_STAGE_FRAGMENT_BIT, module=fs, pName='main'),
    ]
    pipeline = vk.vkCreateGraphicsPipelines(device, None, 1, [vk.VkGraphicsPipelineCreateInfo(
        pStages=stages, pVertexInputState=vertex_input, pInputAssemblyState=input_asm,
        pViewportState=viewport_state, pRasterizationState=raster, pMultisampleState=multisample,
        pColorBlendState=blend, layout=pipe_layout, renderPass=renderpass, subpass=0)], None)[0]

    pool_cmd = vk.vkCreateCommandPool(device, vk.VkCommandPoolCreateInfo(queueFamilyIndex=gfx), None)
    cmd = vk.vkAllocateCommandBuffers(device, vk.VkCommandBufferAllocateInfo(
        commandPool=pool_cmd, level=vk.VK_COMMAND_BUFFER_LEVEL_PRIMARY, commandBufferCount=1))[0]

    vk.vkBeginCommandBuffer(cmd, vk.VkCommandBufferBeginInfo())
    clear = vk.VkClearValue(color=vk.VkClearColorValue(float32=[0.05, 0.05, 0.2, 1.0]))
    vk.vkCmdBeginRenderPass(cmd, vk.VkRenderPassBeginInfo(
        renderPass=renderpass, framebuffer=fb,
        renderArea=vk.VkRect2D(offset=vk.VkOffset2D(x=0, y=0), extent=vk.VkExtent2D(width=W, height=H)),
        pClearValues=[clear]), vk.VK_SUBPASS_CONTENTS_INLINE)
    vk.vkCmdBindPipeline(cmd, vk.VK_PIPELINE_BIND_POINT_GRAPHICS, pipeline)
    vk.vkCmdBindDescriptorSets(cmd, vk.VK_PIPELINE_BIND_POINT_GRAPHICS, pipe_layout, 0, 1, [dset], 0, [])
    vk.vkCmdBindVertexBuffers(cmd, 0, 4, [vb_pos, vb_color, vb_v4, vb_v5], [0, 0, 0, 0])
    vk.vkCmdBindIndexBuffer(cmd, ibuf, 0, vk.VK_INDEX_TYPE_UINT32)
    vk.vkCmdDrawIndexed(cmd, len(idx), 1, 0, 0, 0)
    vk.vkCmdEndRenderPass(cmd)
    vk.vkEndCommandBuffer(cmd)

    vk.vkQueueSubmit(queue, 1, [vk.VkSubmitInfo(pCommandBuffers=[cmd])], None)
    vk.vkQueueWaitIdle(queue)

    # readback: copia a imagem p/ um buffer host-visible e salva PNG
    rb_size = W * H * 4
    rb_buf, rb_mem = alloc_buffer(device, mem_props, rb_size, vk.VK_BUFFER_USAGE_TRANSFER_DST_BIT,
                                   vk.VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT | vk.VK_MEMORY_PROPERTY_HOST_COHERENT_BIT)
    cmd2 = vk.vkAllocateCommandBuffers(device, vk.VkCommandBufferAllocateInfo(
        commandPool=pool_cmd, level=vk.VK_COMMAND_BUFFER_LEVEL_PRIMARY, commandBufferCount=1))[0]
    vk.vkBeginCommandBuffer(cmd2, vk.VkCommandBufferBeginInfo())
    region = vk.VkBufferImageCopy(bufferOffset=0, bufferRowLength=0, bufferImageHeight=0,
        imageSubresource=vk.VkImageSubresourceLayers(aspectMask=vk.VK_IMAGE_ASPECT_COLOR_BIT,
                                                       mipLevel=0, baseArrayLayer=0, layerCount=1),
        imageOffset=vk.VkOffset3D(x=0, y=0, z=0), imageExtent=vk.VkExtent3D(width=W, height=H, depth=1))
    vk.vkCmdCopyImageToBuffer(cmd2, img, vk.VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL, rb_buf, 1, [region])
    vk.vkEndCommandBuffer(cmd2)
    vk.vkQueueSubmit(queue, 1, [vk.VkSubmitInfo(pCommandBuffers=[cmd2])], None)
    vk.vkQueueWaitIdle(queue)

    rb_cffi_buf = vk.vkMapMemory(device, rb_mem, 0, rb_size, 0)
    arr = np.frombuffer(bytes(rb_cffi_buf), dtype=np.uint8).reshape(H, W, 4)
    vk.vkUnmapMemory(device, rb_mem)
    Image.fromarray(arr, 'RGBA').save(OUT_PNG)
    print('salvo em', OUT_PNG)

    vk.vkDeviceWaitIdle(device)
    vk.vkDestroyDevice(device, None)
    vk.vkDestroyInstance(instance, None)


if __name__ == '__main__':
    main()
