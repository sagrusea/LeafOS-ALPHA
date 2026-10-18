use crate::{gdt, memory};
use core::arch::asm;
use x86_64::VirtAddr;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserContextError {
    InvalidEntry,
    InvalidStack,
    EntryNotExecutable,
    StackNotWritable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UserContext {
    entry: VirtAddr,
    stack: VirtAddr,
    argument_count: u64,
    argument_vector: VirtAddr,
}

impl UserContext {
    pub fn new(entry: VirtAddr, stack: VirtAddr) -> Result<Self, UserContextError> {
        Self::with_arguments(entry, stack, 0, VirtAddr::zero())
    }

    pub fn with_arguments(
        entry: VirtAddr,
        stack: VirtAddr,
        argument_count: u64,
        argument_vector: VirtAddr,
    ) -> Result<Self, UserContextError> {
        if !memory::AddressSpace::contains_user_address(entry) {
            return Err(UserContextError::InvalidEntry);
        }
        if !memory::AddressSpace::contains_user_address(stack) || !stack.as_u64().is_multiple_of(16)
        {
            return Err(UserContextError::InvalidStack);
        }
        if argument_count != 0 && !memory::AddressSpace::contains_user_address(argument_vector) {
            return Err(UserContextError::InvalidStack);
        }
        Ok(Self {
            entry,
            stack,
            argument_count,
            argument_vector,
        })
    }

    pub fn for_address_space(
        address_space: &memory::AddressSpace,
        entry: VirtAddr,
        stack: VirtAddr,
    ) -> Result<Self, UserContextError> {
        Self::for_address_space_with_arguments(address_space, entry, stack, 0, VirtAddr::zero())
    }

    pub fn for_address_space_with_arguments(
        address_space: &memory::AddressSpace,
        entry: VirtAddr,
        stack: VirtAddr,
        argument_count: u64,
        argument_vector: VirtAddr,
    ) -> Result<Self, UserContextError> {
        let context = Self::with_arguments(entry, stack, argument_count, argument_vector)?;
        let entry_page = x86_64::structures::paging::Page::containing_address(entry);
        let stack_page =
            x86_64::structures::paging::Page::containing_address(VirtAddr::new(stack.as_u64() - 1));
        let argument_page = x86_64::structures::paging::Page::containing_address(argument_vector);
        if address_space.user_page_access(entry_page) != Some(memory::UserPageAccess::ReadExecute) {
            return Err(UserContextError::EntryNotExecutable);
        }
        if address_space.user_page_access(stack_page) != Some(memory::UserPageAccess::ReadWrite) {
            return Err(UserContextError::StackNotWritable);
        }
        if argument_count != 0
            && address_space.user_page_access(argument_page)
                != Some(memory::UserPageAccess::ReadWrite)
        {
            return Err(UserContextError::StackNotWritable);
        }
        Ok(context)
    }
}

/// Enter ring 3 at a context validated against an address space.
///
/// # Safety
///
/// The caller must have activated that address space, configured the TSS
/// privilege stack and syscall kernel stack, disabled interrupts during the
/// transition, and guarantee both mappings remain present for the user task.
pub unsafe fn enter(context: UserContext) -> ! {
    let entry = context.entry.as_u64();
    let stack = context.stack.as_u64();
    unsafe {
        asm!(
            "mov ax, {user_data}",
            "mov ds, ax",
            "mov es, ax",
            "push {user_data}",
            "push rsi",
            "push 0x202",
            "push {user_code}",
            "push rdi",
            "mov rdi, r8",
            "mov rsi, rdx",
            "swapgs",
            "iretq",
            user_data = const gdt::USER_DATA_SELECTOR,
            user_code = const gdt::USER_CODE_SELECTOR,
            in("rsi") stack,
            in("rdi") entry,
            in("r8") context.argument_count,
            in("rdx") context.argument_vector.as_u64(),
            options(noreturn)
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_case]
    fn user_context_rejects_addresses_outside_user_region() {
        assert_eq!(
            UserContext::new(VirtAddr::new(0x4000), VirtAddr::new(0x8000)),
            Err(UserContextError::InvalidEntry)
        );
        assert_eq!(
            UserContext::new(
                VirtAddr::new(memory::USER_SPACE_START),
                VirtAddr::new(0x4000),
            ),
            Err(UserContextError::InvalidStack)
        );
    }

    #[test_case]
    fn user_context_requires_aligned_stack() {
        let entry = VirtAddr::new(memory::USER_SPACE_START + 0x1000);
        let stack = VirtAddr::new(memory::USER_SPACE_START + 0x2008);
        assert_eq!(
            UserContext::new(entry, stack),
            Err(UserContextError::InvalidStack)
        );
    }
}
