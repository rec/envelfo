use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

thread_local! {
    pub static ALLOCATIONS: Cell<Option<usize>> = const { Cell::new(None) };
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

struct CountingAllocator;

// SAFETY: All operations forward the original allocation arguments to System.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.with(|c| c.set(c.get().map(|n| n + 1)));
        // SAFETY: The caller supplies a valid allocation layout.
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.with(|c| c.set(c.get().map(|n| n + 1)));
        // SAFETY: The caller supplies a valid allocation layout.
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        ALLOCATIONS.with(|c| c.set(c.get().map(|n| n + 1)));
        // SAFETY: The caller supplies a System-owned pointer and its layout.
        unsafe { System.realloc(ptr, layout, size) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        ALLOCATIONS.with(|c| c.set(c.get().map(|n| n + 1)));
        // SAFETY: The caller supplies a System-owned pointer and its layout.
        unsafe { System.dealloc(ptr, layout) }
    }
}
