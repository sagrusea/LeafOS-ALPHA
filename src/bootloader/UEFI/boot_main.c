/**
 * @file efi_bootloader.c
 * @brief Minimal UEFI Bootloader Implementation
 * 
 * Demonstrates: Console output, ASCII<->UCS2 conversion, protocol location,
 * memory allocation, and ExitBootServices sequence.
 */

#include "boot.h"

/* ========================================================================== */
/*                            GLOBAL STATE                                    */
/* ========================================================================== */

EFI_SYSTEM_TABLE  *gST          = NULL;
EFI_BOOT_SERVICES *gBS          = NULL;
EFI_HANDLE         gImageHandle = NULL;

/* ========================================================================== */
/*                          STRING HELPERS                                    */
/* ========================================================================== */

/**
 * @brief Convert an ASCII string to UCS-2 in-place buffer
 * @param ascii  Null-terminated ASCII source string
 * @param ucs2   Destination UCS-2 buffer (caller-allocated)
 * @param max    Maximum number of CHAR16 characters (including null terminator)
 * @return Number of characters written (excluding null terminator)
 */
UINTN AsciiToUcs2(const char *ascii, CHAR16 *ucs2, UINTN max) {
    UINTN i = 0;
    if (!ascii || !ucs2 || max == 0) return 0;

    while (ascii[i] != '\0' && i < (max - 1)) {
        ucs2[i] = (CHAR16)(unsigned char)ascii[i];
        i++;
    }
    ucs2[i] = L'\0';
    return i;
}

/**
 * @brief Print an ASCII string by converting to UCS-2 on the stack
 * @note  Safe for short strings only (< 256 chars). For longer strings,
 *        allocate a buffer and call AsciiToUcs2() directly.
 */
VOID PrintAscii(const char *str) {
    CHAR16 buf[256];
    AsciiToUcs2(str, buf, sizeof(buf) / sizeof(CHAR16));
    gST->ConOut->OutputString(gST->ConOut, buf);
}

/**
 * @brief Compare two EFI_GUIDs
 * @return TRUE if equal, FALSE otherwise
 */
BOOLEAN GuidEqual(const EFI_GUID *a, const EFI_GUID *b) {
    if (!a || !b) return FALSE;
    
    /* Compare as raw bytes to avoid padding issues */
    const UINT8 *pa = (const UINT8 *)a;
    const UINT8 *pb = (const UINT8 *)b;
    for (UINTN i = 0; i < sizeof(EFI_GUID); i++) {
        if (pa[i] != pb[i]) return FALSE;
    }
    return TRUE;
}

/* ========================================================================== */
/*                       MEMORY & LOADING HELPERS                             */
/* ========================================================================== */

/**
 * @brief Allocate zeroed pool memory
 */
EFI_STATUS AllocateZeroPool(EFI_MEMORY_TYPE type, UINTN size, VOID **buffer) {
    EFI_STATUS status = gBS->AllocatePool(type, size, buffer);
    if (!EFI_ERROR(status) && buffer && *buffer) {
        /* Zero manually since UEFI AllocatePool does NOT zero memory */
        UINT8 *p = (UINT8 *)*buffer;
        for (UINTN i = 0; i < size; i++) p[i] = 0;
    }
    return status;
}

/**
 * @brief Locate the Loaded Image Protocol for the current bootloader image
 * @param[out] loaded_image  Pointer to receive the protocol interface
 */
EFI_STATUS GetLoadedImage(EFI_LOADED_IMAGE_PROTOCOL **loaded_image) {
    EFI_GUID guid = EFI_LOADED_IMAGE_PROTOCOL_GUID;
    return gBS->HandleProtocol(gImageHandle, &guid, (VOID **)loaded_image);
}

/* ========================================================================== */
/*                     EXIT BOOT SERVICES SEQUENCE                            */
/* ========================================================================== */

/**
 * @brief Properly exit UEFI boot services before jumping to OS/kernel
 * 
 * This is the most critical and error-prone part of any bootloader.
 * The memory map MUST be retrieved immediately before ExitBootServices,
 * and if it fails with EFI_INVALID_PARAMETER, you must re-get the map
 * and retry (the map key changes on every allocation/free).
 * 
 * @param[in] image_handle  The bootloader's image handle
 * @param[out] mem_map      Receives pointer to final memory map
 * @param[out] mem_map_size Receives size of memory map in bytes
 * @param[out] desc_size    Receives size of each descriptor
 * @return EFI_SUCCESS or error status
 */
EFI_STATUS PrepareExitBootServices(
    EFI_HANDLE image_handle,
    EFI_MEMORY_DESCRIPTOR **mem_map,
    UINTN *mem_map_size,
    UINTN *desc_size
) {
    EFI_STATUS status;
    UINTN map_key = 0;
    UINT32 desc_version = 0;
    UINTN buffer_size = 0;

    /* First call with NULL buffer to get required size */
    status = gBS->GetMemoryMap(&buffer_size, NULL, &map_key, desc_size, &desc_version);
    if (status != EFI_BUFFER_TOO_SMALL) {
        return EFI_LOAD_ERROR;
    }

    /* Allocate extra space: GetMemoryMap itself may allocate, changing the map */
    buffer_size += (*desc_size) * 4;

    status = gBS->AllocatePool(EfiLoaderData, buffer_size, (VOID **)mem_map);
    if (EFI_ERROR(status)) return status;

    /* Retrieve the actual memory map */
    status = gBS->GetMemoryMap(&buffer_size, *mem_map, &map_key, desc_size, &desc_version);
    if (EFI_ERROR(status)) {
        gBS->FreePool(*mem_map);
        *mem_map = NULL;
        return status;
    }

    *mem_map_size = buffer_size;

    /* Attempt to exit boot services */
    status = gBS->ExitBootServices(image_handle, map_key);
    if (EFI_ERROR(status)) {
        /* Map changed between GetMemoryMap and ExitBootServices — free and fail.
         * Caller should NOT retry blindly; this indicates a timing issue. */
        gBS->FreePool(*mem_map);
        *mem_map = NULL;
        return status;
    }

    /* ⚠️ AFTER THIS POINT: No Boot Services calls are valid!
     * Only Runtime Services and direct hardware access are permitted.
     * ConOut, AllocatePool, HandleProtocol, etc. are ALL INVALID. */

    return EFI_SUCCESS;
}

/* ========================================================================== */
/*                           MAIN ENTRY POINT                                 */
/* ========================================================================== */

/**
 * @brief UEFI Application / Bootloader Entry Point
 * 
 * @param ImageHandle   Handle to this loaded image
 * @param SystemTable   Pointer to the EFI System Table
 * @return EFI_STATUS   Return code to firmware
 */
EFI_STATUS EFIAPI efi_main(EFI_HANDLE ImageHandle, EFI_SYSTEM_TABLE *SystemTable) {
    EFI_STATUS status;

    /* Initialize global state FIRST — nothing works without this */
    gST = SystemTable;
    gBS = SystemTable->BootServices;
    gImageHandle = ImageHandle;

    /* Clear screen and set color */
    gST->ConOut->ClearScreen(gST->ConOut);
    gST->ConOut->SetAttribute(gST->ConOut, EFI_TEXT_ATTR(EFI_GREEN, EFI_BLACK));

    PrintAscii("=== LeafOS Bootloader v0.1A ===\r\n");
    gST->ConOut->SetAttribute(gST->ConOut, EFI_TEXT_ATTR(EFI_LIGHTGRAY, EFI_BLACK));

    /* Verify we can access our own loaded image */
    EFI_LOADED_IMAGE_PROTOCOL *loaded_image = NULL;
    status = GetLoadedImage(&loaded_image);
    if (EFI_ERROR(status)) {
        PrintAscii("[FATAL] Failed to locate Loaded Image Protocol\r\n");
        goto halt;
    }

    PrintAscii("[OK] Loaded Image Protocol located\r\n");
    PrintAscii("[INFO] Image Base: ");
    /* Note: Full hex printing requires a formatter; omitted for minimal deps */
    PrintAscii("\r\n");

    /* TODO: Open Simple File System, load kernel ELF/PE from disk
     * TODO: Parse kernel headers, allocate pages, relocate
     * TODO: Set up framebuffer via Graphics Output Protocol
     * TODO: Build kernel command line / boot info struct
     * TODO: Call PrepareExitBootServices() then jump to kernel entry */

    PrintAscii("[INFO] Bootloader initialization complete.\r\n");
    PrintAscii("[INFO] Waiting for keypress to continue...\r\n");

    /* Simple wait-for-key using ConIn (safe while still in boot services) */
    {
        UINTN index = 0;
        /* WaitForEvent is a stub in our minimal header; 
         * in production, implement or use gBS->WaitForEvent properly.
         * For now, just stall briefly as a placeholder. */
        gBS->Stall(3000000); /* 3 seconds */
        (void)index;
    }

halt:
    /* If we reach here without handing off to a kernel, stay in firmware.
     * Returning from efi_main unloads the bootloader image. */
    PrintAscii("[HALT] No kernel loaded. System halted.\r\n");
    
    /* Infinite loop to prevent returning to firmware shell unexpectedly */
    while (TRUE) {
        __asm__ volatile("hlt");
    }

    /* Unreachable, but satisfies compiler */
    return EFI_SUCCESS;
}