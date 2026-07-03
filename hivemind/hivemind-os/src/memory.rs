use bootloader::bootinfo::{MemoryMap, MemoryRegionType};
use x86_64::{
    structures::paging::{
        FrameAllocator, OffsetPageTable, Page, PageTable, PhysFrame, Size4KiB,
    },
    PhysAddr, VirtAddr,
};

/// Initialize the offset page table from the given physical-memory offset.
pub unsafe fn init(physical_memory_offset: VirtAddr) -> OffsetPageTable<'static> {
    let level_4_table = active_level_4_table(physical_memory_offset);
    OffsetPageTable::new(level_4_table, physical_memory_offset)
}

unsafe fn active_level_4_table(physical_memory_offset: VirtAddr) -> &'static mut PageTable {
    use x86_64::registers::control::Cr3;
    let (level_4_table_frame, _) = Cr3::read();
    let phys = level_4_table_frame.start_address();
    let virt = physical_memory_offset + phys.as_u64();
    let page_table_ptr: *mut PageTable = virt.as_mut_ptr();
    &mut *page_table_ptr
}

/// Frame allocator backed by the bootloader's memory map.
///
/// A bump allocator: it remembers the current region and the next physical
/// address within it, so each `allocate_frame` is O(1). The previous version
/// used `usable_frames().nth(next)`, which re-scanned every frame from the start
/// on every call — O(n²) over a boot, adding seconds to startup for a 2 MiB heap.
pub struct BootInfoFrameAllocator {
    memory_map: &'static MemoryMap,
    /// Index of the region we are currently handing frames out of.
    region:     usize,
    /// Next physical address to allocate within the current region (frame-aligned).
    next_addr:  u64,
}

impl BootInfoFrameAllocator {
    pub unsafe fn init(memory_map: &'static MemoryMap) -> Self {
        BootInfoFrameAllocator { memory_map, region: 0, next_addr: 0 }
    }
}

unsafe impl FrameAllocator<Size4KiB> for BootInfoFrameAllocator {
    fn allocate_frame(&mut self) -> Option<PhysFrame> {
        loop {
            let region = self.memory_map.get(self.region)?;

            // Skip non-usable regions.
            if region.region_type != MemoryRegionType::Usable {
                self.region += 1;
                self.next_addr = 0;
                continue;
            }

            let start = region.range.start_addr();
            let end   = region.range.end_addr();

            // Clamp the cursor into this region (region bounds are frame-aligned).
            if self.next_addr < start {
                self.next_addr = start;
            }

            if self.next_addr + 4096 <= end {
                let frame = PhysFrame::containing_address(PhysAddr::new(self.next_addr));
                self.next_addr += 4096;
                return Some(frame);
            }

            // Region exhausted — advance to the next one.
            self.region += 1;
            self.next_addr = 0;
        }
    }
}
