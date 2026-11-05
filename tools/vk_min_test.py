"""Teste minimo do caminho Vulkan nesta maquina (sem compilador C/C++ instalado ainda; usa o
binding Python 'vulkan', ctypes puro sobre a vulkan-1.dll do sistema). Nao desenha nada ainda:
so confirma que instancia -> GPU fisica -> device logico -> fila grafica -> command pool funcionam,
antes de montar o renderizador real (que vai ser C/C++ + SPIR-V, Vulkan e a API escolhida p/ o port).
Uso: python vk_min_test.py
"""
import vulkan as vk


def main():
    app_info = vk.VkApplicationInfo(
        pApplicationName="yw1-port-vktest",
        applicationVersion=vk.VK_MAKE_VERSION(0, 1, 0),
        pEngineName="none",
        engineVersion=vk.VK_MAKE_VERSION(0, 1, 0),
        apiVersion=vk.VK_API_VERSION_1_0,
    )
    inst_info = vk.VkInstanceCreateInfo(pApplicationInfo=app_info)
    instance = vk.vkCreateInstance(inst_info, None)
    print("instancia criada, apiVersion pedida = 1.0 (binding python so expoe esta constante)")

    phys_devices = vk.vkEnumeratePhysicalDevices(instance)
    print("GPUs encontradas:", len(phys_devices))
    chosen = None
    for pd in phys_devices:
        props = vk.vkGetPhysicalDeviceProperties(pd)
        name = props.deviceName
        ver = props.apiVersion
        print("  - %s | apiVersion=%d.%d.%d | tipo=%d" % (
            name, vk.VK_VERSION_MAJOR(ver), vk.VK_VERSION_MINOR(ver), vk.VK_VERSION_PATCH(ver),
            props.deviceType))
        if chosen is None or props.deviceType == vk.VK_PHYSICAL_DEVICE_TYPE_DISCRETE_GPU:
            chosen = pd

    if chosen is None:
        print("nenhuma GPU com suporte a Vulkan; abortando")
        vk.vkDestroyInstance(instance, None)
        return

    queue_families = vk.vkGetPhysicalDeviceQueueFamilyProperties(chosen)
    gfx_family = None
    for i, qf in enumerate(queue_families):
        if qf.queueFlags & vk.VK_QUEUE_GRAPHICS_BIT:
            gfx_family = i
            break
    print("fila grafica: familia #%s (de %d)" % (gfx_family, len(queue_families)))
    if gfx_family is None:
        print("GPU escolhida nao tem fila grafica; abortando")
        vk.vkDestroyInstance(instance, None)
        return

    queue_info = vk.VkDeviceQueueCreateInfo(
        queueFamilyIndex=gfx_family, queueCount=1, pQueuePriorities=[1.0])
    device_info = vk.VkDeviceCreateInfo(pQueueCreateInfos=[queue_info])
    device = vk.vkCreateDevice(chosen, device_info, None)
    queue = vk.vkGetDeviceQueue(device, gfx_family, 0)
    print("device logico + fila grafica criados com sucesso (handle de fila: %r)" % (queue,))

    pool_info = vk.VkCommandPoolCreateInfo(queueFamilyIndex=gfx_family)
    pool = vk.vkCreateCommandPool(device, pool_info, None)
    alloc_info = vk.VkCommandBufferAllocateInfo(
        commandPool=pool, level=vk.VK_COMMAND_BUFFER_LEVEL_PRIMARY, commandBufferCount=1)
    cmdbuf = vk.vkAllocateCommandBuffers(device, alloc_info)[0]
    print("command pool + command buffer alocados OK")

    vk.vkDestroyCommandPool(device, pool, None)
    vk.vkDestroyDevice(device, None)
    vk.vkDestroyInstance(instance, None)
    print("tudo destruido sem erro -> caminho Vulkan OK nesta maquina.")


if __name__ == "__main__":
    main()
