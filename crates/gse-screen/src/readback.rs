//! Guest side: GPU buffers the compositor copies frames into, and the
//! Vulkan copy that brings each frame back into system memory.
//!
//! Screencopy into a `wl_shm` buffer makes the compositor read the frame back
//! itself, and on NVIDIA it offers only a 24-bit format that the driver
//! converts on the CPU (over 100 ms for a 1440p frame). Into a dmabuf the
//! compositor only blits on the GPU. The targets here are `B8G8R8A8` images
//! exported as dmabufs (`XRGB8888` to the compositor) in one of the GPU's own
//! tiled layouts the compositor can render into; NVIDIA renders into no linear
//! buffer. After each copy the image is transferred, de-tiled by the copy,
//! into a host-visible buffer and from there into the shared region.

use std::ffi::CStr;
use std::os::fd::{FromRawFd as _, OwnedFd};

use ash::{ext, khr, vk};

/// DRM's `XRGB8888`: bytes B, G, R, X, the layout of `B8G8R8A8`.
pub const DRM_FORMAT_XRGB8888: u32 = 0x3432_5258;
const DRM_FORMAT_MOD_LINEAR: u64 = 0;
const DRM_FORMAT_MOD_INVALID: u64 = 0x00ff_ffff_ffff_ffff;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("loading Vulkan: {0}")]
    Load(#[from] ash::LoadingError),
    #[error("Vulkan: {0}")]
    Vulkan(#[from] vk::Result),
    #[error("{0}")]
    Unsupported(&'static str),
}

/// A GPU image the compositor copies frames into.
pub struct Target {
    image: vk::Image,
    memory: vk::DeviceMemory,
    /// The dmabuf to hand to the compositor.
    pub fd: OwnedFd,
    pub offset: u32,
    pub stride: u32,
    pub modifier: u64,
}

pub struct Readback {
    _entry: ash::Entry,
    instance: ash::Instance,
    device: ash::Device,
    queue: vk::Queue,
    family: u32,
    pool: vk::CommandPool,
    command: vk::CommandBuffer,
    fence: vk::Fence,
    staging: vk::Buffer,
    staging_memory: vk::DeviceMemory,
    staging_coherent: bool,
    staging_bytes: *const u8,
    pub width: u32,
    pub height: u32,
    pub targets: Vec<Target>,
}

fn memory_type(
    props: &vk::PhysicalDeviceMemoryProperties,
    bits: u32,
    want: vk::MemoryPropertyFlags,
) -> Option<u32> {
    (0..props.memory_type_count).find(|&i| {
        bits & (1 << i) != 0 && props.memory_types[i as usize].property_flags.contains(want)
    })
}

impl Readback {
    /// Sets up `count` targets of `width` x `height` on the first discrete
    /// GPU, or the first GPU when there is no discrete one, in a layout from
    /// `modifiers`: the ones the compositor accepts for `XRGB8888`.
    pub fn new(width: u32, height: u32, count: usize, modifiers: &[u64]) -> Result<Self, Error> {
        let modifiers: Vec<u64> = modifiers
            .iter()
            .copied()
            .filter(|&m| m != DRM_FORMAT_MOD_LINEAR && m != DRM_FORMAT_MOD_INVALID)
            .collect();
        if modifiers.is_empty() {
            return Err(Error::Unsupported(
                "the compositor accepts no tiled XRGB8888",
            ));
        }
        // SAFETY: every Vulkan call below follows the specification's valid
        // usage for the objects created here; they live until `Drop`.
        unsafe {
            let entry = ash::Entry::load()?;
            let app = vk::ApplicationInfo::default().api_version(vk::API_VERSION_1_3);
            let instance = entry.create_instance(
                &vk::InstanceCreateInfo::default().application_info(&app),
                None,
            )?;
            let (gpu, family) = pick_device(&instance)?;

            let extensions: [&CStr; 4] = [
                khr::external_memory_fd::NAME,
                ext::external_memory_dma_buf::NAME,
                ext::image_drm_format_modifier::NAME,
                ext::queue_family_foreign::NAME,
            ];
            let extension_names: Vec<_> = extensions.iter().map(|e| e.as_ptr()).collect();
            let priorities = [1.0];
            let queues = [vk::DeviceQueueCreateInfo::default()
                .queue_family_index(family)
                .queue_priorities(&priorities)];
            let device = instance.create_device(
                gpu,
                &vk::DeviceCreateInfo::default()
                    .queue_create_infos(&queues)
                    .enabled_extension_names(&extension_names),
                None,
            )?;
            let queue = device.get_device_queue(family, 0);
            let memory_props = instance.get_physical_device_memory_properties(gpu);

            let mut targets = Vec::with_capacity(count);
            for _ in 0..count {
                targets.push(create_target(
                    &instance,
                    &device,
                    &memory_props,
                    (width, height),
                    &modifiers,
                )?);
            }

            let (staging, staging_memory, staging_coherent, staging_bytes) = create_staging(
                &device,
                &memory_props,
                u64::from(width) * u64::from(height) * 4,
            )?;

            let pool = device.create_command_pool(
                &vk::CommandPoolCreateInfo::default()
                    .queue_family_index(family)
                    .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER),
                None,
            )?;
            let command = device.allocate_command_buffers(
                &vk::CommandBufferAllocateInfo::default()
                    .command_pool(pool)
                    .level(vk::CommandBufferLevel::PRIMARY)
                    .command_buffer_count(1),
            )?[0];
            let fence = device.create_fence(&vk::FenceCreateInfo::default(), None)?;
            Ok(Self {
                _entry: entry,
                instance,
                device,
                queue,
                family,
                pool,
                command,
                fence,
                staging,
                staging_memory,
                staging_coherent,
                staging_bytes,
                width,
                height,
                targets,
            })
        }
    }

    /// Copies the frame the compositor left in target `index` into `out` as
    /// tightly packed `B8G8R8A8` rows. Returns the byte count written.
    pub fn read(&mut self, index: usize, out: &mut [u8]) -> Result<usize, Error> {
        let size = self.width as usize * self.height as usize * 4;
        let out = out
            .get_mut(..size)
            .ok_or(Error::Unsupported("frame larger than a slot"))?;
        let image = self.targets[index].image;
        let range = vk::ImageSubresourceRange::default()
            .aspect_mask(vk::ImageAspectFlags::COLOR)
            .level_count(1)
            .layer_count(1);
        // SAFETY: the command buffer, image, buffer and fence belong to this
        // device; the fence wait finishes the GPU's use of them before the
        // mapped staging memory is read.
        unsafe {
            let d = &self.device;
            d.begin_command_buffer(
                self.command,
                &vk::CommandBufferBeginInfo::default()
                    .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
            )?;
            // The compositor wrote the image outside Vulkan: acquire it from
            // the foreign queue family.
            let acquire = vk::ImageMemoryBarrier::default()
                .src_access_mask(vk::AccessFlags::empty())
                .dst_access_mask(vk::AccessFlags::TRANSFER_READ)
                .old_layout(vk::ImageLayout::GENERAL)
                .new_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
                .src_queue_family_index(vk::QUEUE_FAMILY_FOREIGN_EXT)
                .dst_queue_family_index(self.family)
                .image(image)
                .subresource_range(range);
            d.cmd_pipeline_barrier(
                self.command,
                vk::PipelineStageFlags::TOP_OF_PIPE,
                vk::PipelineStageFlags::TRANSFER,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[acquire],
            );
            let region = vk::BufferImageCopy::default()
                .image_subresource(
                    vk::ImageSubresourceLayers::default()
                        .aspect_mask(vk::ImageAspectFlags::COLOR)
                        .layer_count(1),
                )
                .image_extent(vk::Extent3D {
                    width: self.width,
                    height: self.height,
                    depth: 1,
                });
            d.cmd_copy_image_to_buffer(
                self.command,
                image,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                self.staging,
                &[region],
            );
            let to_host = vk::BufferMemoryBarrier::default()
                .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                .dst_access_mask(vk::AccessFlags::HOST_READ)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .buffer(self.staging)
                .size(vk::WHOLE_SIZE);
            // Hand the image back to the compositor for the next frame.
            let release = vk::ImageMemoryBarrier::default()
                .src_access_mask(vk::AccessFlags::TRANSFER_READ)
                .dst_access_mask(vk::AccessFlags::empty())
                .old_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
                .new_layout(vk::ImageLayout::GENERAL)
                .src_queue_family_index(self.family)
                .dst_queue_family_index(vk::QUEUE_FAMILY_FOREIGN_EXT)
                .image(image)
                .subresource_range(range);
            d.cmd_pipeline_barrier(
                self.command,
                vk::PipelineStageFlags::TRANSFER,
                vk::PipelineStageFlags::HOST | vk::PipelineStageFlags::BOTTOM_OF_PIPE,
                vk::DependencyFlags::empty(),
                &[],
                &[to_host],
                &[release],
            );
            d.end_command_buffer(self.command)?;
            let commands = [self.command];
            d.queue_submit(
                self.queue,
                &[vk::SubmitInfo::default().command_buffers(&commands)],
                self.fence,
            )?;
            d.wait_for_fences(&[self.fence], true, u64::MAX)?;
            d.reset_fences(&[self.fence])?;
            if !self.staging_coherent {
                d.invalidate_mapped_memory_ranges(&[vk::MappedMemoryRange::default()
                    .memory(self.staging_memory)
                    .size(vk::WHOLE_SIZE)])?;
            }
            out.copy_from_slice(std::slice::from_raw_parts(self.staging_bytes, size));
        }
        Ok(size)
    }
}

/// The first discrete GPU, or the first GPU when there is no discrete one,
/// and a queue family of it that can transfer.
///
/// # Safety
///
/// `instance` must be a valid Vulkan instance.
unsafe fn pick_device(instance: &ash::Instance) -> Result<(vk::PhysicalDevice, u32), Error> {
    // SAFETY: the caller's contract.
    unsafe {
        let gpus = instance.enumerate_physical_devices()?;
        let gpu = gpus
            .iter()
            .copied()
            .find(|&g| {
                instance.get_physical_device_properties(g).device_type
                    == vk::PhysicalDeviceType::DISCRETE_GPU
            })
            .or_else(|| gpus.first().copied())
            .ok_or(Error::Unsupported("no Vulkan device"))?;
        let family = instance
            .get_physical_device_queue_family_properties(gpu)
            .iter()
            .position(|f| {
                f.queue_flags.contains(vk::QueueFlags::TRANSFER)
                    || f.queue_flags.contains(vk::QueueFlags::GRAPHICS)
            })
            .ok_or(Error::Unsupported("no transfer queue"))?;
        let family = u32::try_from(family).map_err(|_| Error::Unsupported("queue family"))?;
        Ok((gpu, family))
    }
}

/// Creates one target image in a layout from `modifiers` and exports it as
/// a dmabuf.
///
/// # Safety
///
/// `device` must belong to `instance` and have the dmabuf and modifier
/// extensions enabled.
unsafe fn create_target(
    instance: &ash::Instance,
    device: &ash::Device,
    memory_props: &vk::PhysicalDeviceMemoryProperties,
    (width, height): (u32, u32),
    modifiers: &[u64],
) -> Result<Target, Error> {
    // SAFETY: the caller's contract; the objects belong to `device`.
    unsafe {
        let fd_api = khr::external_memory_fd::Device::new(instance, device);
        let modifier_api = ext::image_drm_format_modifier::Device::new(instance, device);
        let memory_props = *memory_props;
        let mut modifier_list =
            vk::ImageDrmFormatModifierListCreateInfoEXT::default().drm_format_modifiers(modifiers);
        let mut external = vk::ExternalMemoryImageCreateInfo::default()
            .handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);
        let image = device.create_image(
            &vk::ImageCreateInfo::default()
                .image_type(vk::ImageType::TYPE_2D)
                .format(vk::Format::B8G8R8A8_UNORM)
                .extent(vk::Extent3D {
                    width,
                    height,
                    depth: 1,
                })
                .mip_levels(1)
                .array_layers(1)
                .samples(vk::SampleCountFlags::TYPE_1)
                .tiling(vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT)
                .usage(
                    vk::ImageUsageFlags::TRANSFER_SRC
                        | vk::ImageUsageFlags::TRANSFER_DST
                        | vk::ImageUsageFlags::COLOR_ATTACHMENT
                        | vk::ImageUsageFlags::SAMPLED,
                )
                .sharing_mode(vk::SharingMode::EXCLUSIVE)
                .initial_layout(vk::ImageLayout::UNDEFINED)
                .push_next(&mut modifier_list)
                .push_next(&mut external),
            None,
        )?;
        let requirements = device.get_image_memory_requirements(image);
        let type_index = memory_type(
            &memory_props,
            requirements.memory_type_bits,
            vk::MemoryPropertyFlags::DEVICE_LOCAL,
        )
        .or_else(|| {
            memory_type(
                &memory_props,
                requirements.memory_type_bits,
                vk::MemoryPropertyFlags::empty(),
            )
        })
        .ok_or(Error::Unsupported("no memory for the target image"))?;
        let mut export = vk::ExportMemoryAllocateInfo::default()
            .handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);
        let mut dedicated = vk::MemoryDedicatedAllocateInfo::default().image(image);
        let memory = device.allocate_memory(
            &vk::MemoryAllocateInfo::default()
                .allocation_size(requirements.size)
                .memory_type_index(type_index)
                .push_next(&mut export)
                .push_next(&mut dedicated),
            None,
        )?;
        device.bind_image_memory(image, memory, 0)?;
        let raw = fd_api.get_memory_fd(
            &vk::MemoryGetFdInfoKHR::default()
                .memory(memory)
                .handle_type(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT),
        )?;
        let fd = OwnedFd::from_raw_fd(raw);
        let layout = device.get_image_subresource_layout(
            image,
            vk::ImageSubresource::default().aspect_mask(vk::ImageAspectFlags::MEMORY_PLANE_0_EXT),
        );
        let mut props = vk::ImageDrmFormatModifierPropertiesEXT::default();
        modifier_api.get_image_drm_format_modifier_properties(image, &mut props)?;
        Ok(Target {
            image,
            memory,
            fd,
            offset: u32::try_from(layout.offset).map_err(|_| Error::Unsupported("offset"))?,
            stride: u32::try_from(layout.row_pitch).map_err(|_| Error::Unsupported("stride"))?,
            modifier: props.drm_format_modifier,
        })
    }
}

/// Creates the host-visible buffer frames are copied into, mapped: the
/// buffer, its memory, whether that memory is coherent, and the mapping.
///
/// # Safety
///
/// `device` must be a valid device described by `memory_props`.
unsafe fn create_staging(
    device: &ash::Device,
    memory_props: &vk::PhysicalDeviceMemoryProperties,
    size: u64,
) -> Result<(vk::Buffer, vk::DeviceMemory, bool, *const u8), Error> {
    // SAFETY: the caller's contract; the objects belong to `device`.
    unsafe {
        let memory_props = *memory_props;
        let staging = device.create_buffer(
            &vk::BufferCreateInfo::default()
                .size(size)
                .usage(vk::BufferUsageFlags::TRANSFER_DST)
                .sharing_mode(vk::SharingMode::EXCLUSIVE),
            None,
        )?;
        let requirements = device.get_buffer_memory_requirements(staging);
        let cached = vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_CACHED;
        let coherent =
            vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT;
        let type_index = memory_type(&memory_props, requirements.memory_type_bits, cached)
            .or_else(|| memory_type(&memory_props, requirements.memory_type_bits, coherent))
            .ok_or(Error::Unsupported("no host-visible memory"))?;
        let staging_coherent = memory_props.memory_types[type_index as usize]
            .property_flags
            .contains(vk::MemoryPropertyFlags::HOST_COHERENT);
        let staging_memory = device.allocate_memory(
            &vk::MemoryAllocateInfo::default()
                .allocation_size(requirements.size)
                .memory_type_index(type_index),
            None,
        )?;
        device.bind_buffer_memory(staging, staging_memory, 0)?;
        let staging_bytes = device
            .map_memory(
                staging_memory,
                0,
                vk::WHOLE_SIZE,
                vk::MemoryMapFlags::empty(),
            )?
            .cast::<u8>()
            .cast_const();
        Ok((staging, staging_memory, staging_coherent, staging_bytes))
    }
}

impl Drop for Readback {
    fn drop(&mut self) {
        // SAFETY: nothing is in flight after `read` waited on its fence; the
        // objects are destroyed child first.
        unsafe {
            let d = &self.device;
            d.device_wait_idle().ok();
            d.destroy_fence(self.fence, None);
            d.destroy_command_pool(self.pool, None);
            d.unmap_memory(self.staging_memory);
            d.destroy_buffer(self.staging, None);
            d.free_memory(self.staging_memory, None);
            for target in &self.targets {
                d.destroy_image(target.image, None);
                d.free_memory(target.memory, None);
            }
            d.destroy_device(None);
            self.instance.destroy_instance(None);
        }
    }
}
