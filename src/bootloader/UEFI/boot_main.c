#include "boot.h"

#ifndef EFI_PAGE_SIZE
#define EFI_PAGE_SIZE 4096ULL
#endif

/* ========================================================================== */
/*                              KERNEL SETTINGS                               */
/* ========================================================================== */

/* Your Rust kernel will be loaded/reserved at 2 MiB. */
#define KERNEL_BASE_ADDRESS     0x00200000ULL
#define PAGE_TABLE_ENTRY_PRESENT 0x001ULL
#define PAGE_TABLE_ENTRY_WRITABLE 0x002ULL
#define PAGE_TABLE_ENTRY_HUGE    0x080ULL
#define PAGE_TABLE_ADDRESS_MASK  0x000FFFFFFFFFF000ULL
#define PAGE_TABLE_ENTRY_COUNT   512
#define PAGE_TABLE_ENTRY_NO_EXECUTE (1ULL << 63)
#define PAGE_TABLE_SIZE          4096ULL
#define HUGE_PAGE_SIZE           (2ULL * 1024 * 1024)
#define PHYSICAL_MAP_WINDOW_SIZE (512ULL * 1024 * 1024 * 1024)
#define EFI_LOADER_CODE_TYPE 1
#define EFI_LOADER_DATA_TYPE 2
#define EFI_BOOT_SERVICES_CODE_TYPE 3
#define EFI_BOOT_SERVICES_DATA_TYPE 4
#define EFI_RUNTIME_SERVICES_CODE_TYPE 5
#define EFI_RUNTIME_SERVICES_DATA_TYPE 6
#define EFI_CONVENTIONAL_MEMORY_TYPE 7
#define EFI_ACPI_RECLAIM_MEMORY_TYPE 9
#define EFI_ACPI_NVS_TYPE 10
#define EFI_PAL_CODE_TYPE 13
#define EFI_PERSISTENT_MEMORY_TYPE 14

#ifndef EFI_OUT_OF_RESOURCES
#define EFI_OUT_OF_RESOURCES 0x8000000000000009ULL
#endif

/*
 * Reserve space for kernel + BSS.
 *
 * IMPORTANT: on OVMF, the free memory region starting at KERNEL_BASE_ADDRESS
 * (0x200000) typically ends at 0x800000, where OVMF places its own page
 * tables -- this is a fixed low address, not RAM-size dependent, so giving
 * QEMU more memory does NOT move it. That leaves a hard ceiling of 6 MiB
 * (1536 pages) available at 0x200000 on OVMF, regardless of -m.
 *
 * 1280 pages = 5 MiB, leaving a little headroom below that ceiling.
 * If your kernel image + BSS needs more than this, either shrink it or
 * move KERNEL_BASE_ADDRESS above OVMF's reserved region (e.g. 0x800000+)
 * and load there instead.
 */
#define KERNEL_RESERVE_PAGES    1280

/*
 * Calling convention used to enter the Rust kernel.
 *
 * Default: System V ABI, matching normal Rust `extern "C"` on x86_64.
 *
 * If your Rust kernel entry uses UEFI calling convention, compile with:
 *   -DKERNEL_CALL_ABI=EFIAPI
 */
#if defined(__GNUC__) || defined(__clang__)
#define SYSV_ABI __attribute__((sysv_abi))
#else
#define SYSV_ABI
#endif

#ifndef KERNEL_CALL_ABI
#define KERNEL_CALL_ABI SYSV_ABI
#endif

/* ========================================================================== */
/*                          MISSING UEFI DEFINITIONS                          */
/* ========================================================================== */

#ifndef EFIAPI
#if defined(__GNUC__) || defined(__clang__)
#define EFIAPI __attribute__((ms_abi))
#else
#define EFIAPI
#endif
#endif

#ifndef EFI_FILE_MODE_READ
#define EFI_FILE_MODE_READ      0x0000000000000001ULL
#endif

#ifndef EFI_SIMPLE_FILE_SYSTEM_PROTOCOL_GUID
#define EFI_SIMPLE_FILE_SYSTEM_PROTOCOL_GUID \
    EFI_GUID(0x964E5B22, 0x6459, 0x11D2, 0x8E, 0x39, 0x00, 0xA0, 0xC9, 0x69, 0x72, 0x3B)
#endif

#ifndef EFI_FILE_INFO_GUID
#define EFI_FILE_INFO_GUID \
    EFI_GUID(0x09576E92, 0x6D3F, 0x11D2, 0x8E, 0x39, 0x00, 0xA0, 0xC9, 0x69, 0x72, 0x3B)
#endif

#ifndef EFI_LOADED_IMAGE_PROTOCOL_GUID
#define EFI_LOADED_IMAGE_PROTOCOL_GUID \
    EFI_GUID(0x5B1B31A1, 0x9562, 0x11D2, 0x8E, 0x3F, 0x00, 0xA0, 0xC9, 0x69, 0x72, 0x3B)
#endif

typedef struct {
    UINT16 Year;
    UINT8  Month;
    UINT8  Day;
    UINT8  Hour;
    UINT8  Minute;
    UINT8  Second;
    UINT8  Pad1;
    UINT32 Nanosecond;
    INT16  TimeZone;
    UINT8  Daylight;
    UINT8  Pad2;
} EFI_TIME;

typedef struct {
    UINT64    Size;
    UINT64    FileSize;
    UINT64    PhysicalSize;
    EFI_TIME  CreateTime;
    EFI_TIME  LastAccessTime;
    EFI_TIME  LastModificationTime;
    UINT64    Attribute;
    CHAR16    FileName[1];
} EFI_FILE_INFO;

typedef struct _EFI_FILE_PROTOCOL EFI_FILE_PROTOCOL;
typedef struct _EFI_SIMPLE_FILE_SYSTEM_PROTOCOL EFI_SIMPLE_FILE_SYSTEM_PROTOCOL;

struct _EFI_FILE_PROTOCOL {
    UINT64 Revision;

    EFI_STATUS (EFIAPI *Open)(
        EFI_FILE_PROTOCOL *Self,
        EFI_FILE_PROTOCOL **NewHandle,
        CHAR16 *FileName,
        UINT64 OpenMode,
        UINT64 Attributes
    );

    EFI_STATUS (EFIAPI *Close)(
        EFI_FILE_PROTOCOL *Self
    );

    EFI_STATUS (EFIAPI *Delete)(
        EFI_FILE_PROTOCOL *Self
    );

    EFI_STATUS (EFIAPI *Read)(
        EFI_FILE_PROTOCOL *Self,
        UINTN *BufferSize,
        VOID *Buffer
    );

    EFI_STATUS (EFIAPI *Write)(
        EFI_FILE_PROTOCOL *Self,
        UINTN *BufferSize,
        VOID *Buffer
    );

    EFI_STATUS (EFIAPI *GetPosition)(
        EFI_FILE_PROTOCOL *Self,
        UINT64 *Position
    );

    EFI_STATUS (EFIAPI *SetPosition)(
        EFI_FILE_PROTOCOL *Self,
        UINT64 Position
    );

    EFI_STATUS (EFIAPI *GetInfo)(
        EFI_FILE_PROTOCOL *Self,
        EFI_GUID *InformationType,
        UINTN *BufferSize,
        VOID *Buffer
    );

    EFI_STATUS (EFIAPI *SetInfo)(
        EFI_FILE_PROTOCOL *Self,
        EFI_GUID *InformationType,
        UINTN BufferSize,
        VOID *Buffer
    );

    EFI_STATUS (EFIAPI *Flush)(
        EFI_FILE_PROTOCOL *Self
    );
};

struct _EFI_SIMPLE_FILE_SYSTEM_PROTOCOL {
    UINT64 Revision;

    EFI_STATUS (EFIAPI *OpenVolume)(
        EFI_SIMPLE_FILE_SYSTEM_PROTOCOL *Self,
        EFI_FILE_PROTOCOL **Root
    );
};

/* ========================================================================== */
/*                              BOOT INFO STRUCT                              */
/* ========================================================================== */

#define BOOT_INFO_MAGIC 0x52555354424F4F54ULL
#define INITRD_MAX_SIZE (16ULL * 1024ULL * 1024ULL)

typedef struct {
    UINT64 Magic;
    UINT32 Version;
    UINT32 Padding0;

    EFI_SYSTEM_TABLE *SystemTable;

    EFI_MEMORY_DESCRIPTOR *MemoryMap;
    UINTN MemoryMapSize;
    UINTN DescriptorSize;
    UINT32 DescriptorVersion;
    UINT32 Padding1;

    UINT64 KernelBase;
    UINT64 KernelFileSize;
    UINT64 KernelReservedSize;
    UINT64 PageTableRoot;
    UINT64 PhysicalMemoryOffset;

    /* Graphics Output Protocol framebuffer, valid only if FramebufferBase != 0 */
    UINT64 FramebufferBase;
    UINT64 FramebufferSize;
    UINT32 FramebufferWidth;
    UINT32 FramebufferHeight;
    UINT32 FramebufferPixelsPerScanLine;
    UINT32 FramebufferPixelFormat;   /* EFI_GRAPHICS_PIXEL_FORMAT */

    VOID *InitrdBase;
    UINTN InitrdSize;
} BootInfo;

/* ========================================================================== */
/*                                GLOBALS                                     */
/* ========================================================================== */

EFI_SYSTEM_TABLE *gST = NULL;
EFI_BOOT_SERVICES *gBS = NULL;
EFI_HANDLE gImageHandle = NULL;
static EFI_PHYSICAL_ADDRESS gOriginalPageTableRoot = 0;
static EFI_PHYSICAL_ADDRESS gKernelPageTableRoot = 0;
static EFI_PHYSICAL_ADDRESS gPhysicalMemoryOffset = 0;
static UINTN gPhysicalMemoryPml4Index = 0;

/* ========================================================================== */
/*                                 HELPERS                                    */
/* ========================================================================== */

static VOID ZeroMemory(volatile VOID *Destination, UINTN Size)
{
    volatile UINT8 *p = (volatile UINT8 *)Destination;

    while (Size--) {
        *p++ = 0;
    }
}

static EFI_STATUS ClonePageTableLevel(
    EFI_PHYSICAL_ADDRESS SourceAddress,
    EFI_PHYSICAL_ADDRESS DestinationAddress,
    UINTN Level
)
{
    EFI_STATUS Status;
    UINT64 *Source = (UINT64 *)(UINTN)SourceAddress;
    UINT64 *Destination = (UINT64 *)(UINTN)DestinationAddress;

    for (UINTN Index = 0; Index < PAGE_TABLE_ENTRY_COUNT; Index++) {
        UINT64 Entry = Source[Index];
        EFI_PHYSICAL_ADDRESS ChildAddress;
        EFI_PHYSICAL_ADDRESS ClonedChildAddress;

        Destination[Index] = Entry;
        if ((Entry & PAGE_TABLE_ENTRY_PRESENT) == 0 || Level == 1) {
            continue;
        }

        ChildAddress = Entry & PAGE_TABLE_ADDRESS_MASK;
        if (ChildAddress == gOriginalPageTableRoot) {
            ClonedChildAddress = gKernelPageTableRoot;
        } else if ((Entry & PAGE_TABLE_ENTRY_HUGE) != 0) {
            continue;
        } else {
            Status = gBS->AllocatePages(
                EFI_ALLOCATE_ANY_PAGES,
                EfiLoaderData,
                1,
                &ClonedChildAddress
            );
            if (EFI_ERROR(Status)) {
                return Status;
            }

            Status = ClonePageTableLevel(
                ChildAddress,
                ClonedChildAddress,
                Level - 1
            );
            if (EFI_ERROR(Status)) {
                return Status;
            }
        }

        Destination[Index] =
            (Entry & ~PAGE_TABLE_ADDRESS_MASK) |
            (ClonedChildAddress & PAGE_TABLE_ADDRESS_MASK);
    }

    return EFI_SUCCESS;
}

static EFI_STATUS AllocateZeroedPage(EFI_PHYSICAL_ADDRESS *Address)
{
    EFI_STATUS Status = gBS->AllocatePages(
        EFI_ALLOCATE_ANY_PAGES,
        EfiLoaderData,
        1,
        Address
    );
    if (EFI_ERROR(Status)) {
        return Status;
    }

    ZeroMemory((volatile VOID *)(UINTN)*Address, PAGE_TABLE_SIZE);
    return EFI_SUCCESS;
}

static EFI_STATUS EnsurePhysicalPageDirectory(UINT64 PhysicalAddress, UINT64 **Level2)
{
    EFI_STATUS Status;
    UINT64 *Level4 = (UINT64 *)(UINTN)gKernelPageTableRoot;
    UINT64 *Level3;
    EFI_PHYSICAL_ADDRESS NewPage;
    UINTN Level3Index = (UINTN)((PhysicalAddress >> 30) & 0x1FF);

    if ((Level4[gPhysicalMemoryPml4Index] & PAGE_TABLE_ENTRY_PRESENT) == 0) {
        Status = AllocateZeroedPage(&NewPage);
        if (EFI_ERROR(Status)) {
            return Status;
        }
        Level4[gPhysicalMemoryPml4Index] =
            (NewPage & PAGE_TABLE_ADDRESS_MASK) |
            PAGE_TABLE_ENTRY_PRESENT |
            PAGE_TABLE_ENTRY_WRITABLE;
    }

    Level3 = (UINT64 *)(UINTN)(
        Level4[gPhysicalMemoryPml4Index] & PAGE_TABLE_ADDRESS_MASK
    );
    if ((Level3[Level3Index] & PAGE_TABLE_ENTRY_PRESENT) == 0) {
        Status = AllocateZeroedPage(&NewPage);
        if (EFI_ERROR(Status)) {
            return Status;
        }
        Level3[Level3Index] =
            (NewPage & PAGE_TABLE_ADDRESS_MASK) |
            PAGE_TABLE_ENTRY_PRESENT |
            PAGE_TABLE_ENTRY_WRITABLE;
    }
    if ((Level3[Level3Index] & PAGE_TABLE_ENTRY_HUGE) != 0) {
        return EFI_ACCESS_DENIED;
    }

    *Level2 = (UINT64 *)(UINTN)(Level3[Level3Index] & PAGE_TABLE_ADDRESS_MASK);
    return EFI_SUCCESS;
}

static EFI_STATUS EnsurePhysicalPageTable(UINT64 PhysicalAddress, UINT64 **PageTable)
{
    EFI_STATUS Status;
    UINT64 *Level2;
    EFI_PHYSICAL_ADDRESS NewPage;
    UINTN Level2Index = (UINTN)((PhysicalAddress >> 21) & 0x1FF);

    Status = EnsurePhysicalPageDirectory(PhysicalAddress, &Level2);
    if (EFI_ERROR(Status)) {
        return Status;
    }
    if ((Level2[Level2Index] & PAGE_TABLE_ENTRY_PRESENT) == 0) {
        Status = AllocateZeroedPage(&NewPage);
        if (EFI_ERROR(Status)) {
            return Status;
        }
        Level2[Level2Index] =
            (NewPage & PAGE_TABLE_ADDRESS_MASK) |
            PAGE_TABLE_ENTRY_PRESENT |
            PAGE_TABLE_ENTRY_WRITABLE;
    }
    if ((Level2[Level2Index] & PAGE_TABLE_ENTRY_HUGE) != 0) {
        return EFI_ACCESS_DENIED;
    }

    *PageTable = (UINT64 *)(UINTN)(Level2[Level2Index] & PAGE_TABLE_ADDRESS_MASK);
    return EFI_SUCCESS;
}

static BOOLEAN IsDirectMappedRamType(UINT32 Type)
{
    switch (Type) {
        case EFI_LOADER_CODE_TYPE:
        case EFI_LOADER_DATA_TYPE:
        case EFI_BOOT_SERVICES_CODE_TYPE:
        case EFI_BOOT_SERVICES_DATA_TYPE:
        case EFI_RUNTIME_SERVICES_CODE_TYPE:
        case EFI_RUNTIME_SERVICES_DATA_TYPE:
        case EFI_CONVENTIONAL_MEMORY_TYPE:
        case EFI_ACPI_RECLAIM_MEMORY_TYPE:
        case EFI_ACPI_NVS_TYPE:
        case EFI_PAL_CODE_TYPE:
        case EFI_PERSISTENT_MEMORY_TYPE:
            return TRUE;
        default:
            return FALSE;
    }
}

static EFI_STATUS MapPhysicalPage4KiB(UINT64 PhysicalAddress)
{
    EFI_STATUS Status;
    UINT64 *PageTable;
    UINTN PageIndex = (UINTN)((PhysicalAddress >> 12) & 0x1FF);

    Status = EnsurePhysicalPageTable(PhysicalAddress, &PageTable);
    if (EFI_ERROR(Status)) {
        return Status;
    }
    if ((PageTable[PageIndex] & PAGE_TABLE_ENTRY_PRESENT) != 0) {
        return EFI_ACCESS_DENIED;
    }

    PageTable[PageIndex] =
        (PhysicalAddress & PAGE_TABLE_ADDRESS_MASK) |
        PAGE_TABLE_ENTRY_PRESENT |
        PAGE_TABLE_ENTRY_WRITABLE |
        PAGE_TABLE_ENTRY_NO_EXECUTE;
    return EFI_SUCCESS;
}

static EFI_STATUS MapPhysicalPage2MiB(UINT64 PhysicalAddress)
{
    EFI_STATUS Status;
    UINT64 *Level2;
    UINTN Level2Index = (UINTN)((PhysicalAddress >> 21) & 0x1FF);

    Status = EnsurePhysicalPageDirectory(PhysicalAddress, &Level2);
    if (EFI_ERROR(Status)) {
        return Status;
    }

    if ((Level2[Level2Index] & PAGE_TABLE_ENTRY_PRESENT) != 0) {
        if ((Level2[Level2Index] & PAGE_TABLE_ENTRY_HUGE) != 0) {
            return EFI_ACCESS_DENIED;
        }

        for (UINTN Index = 0; Index < PAGE_TABLE_ENTRY_COUNT; Index++) {
            UINT64 PhysicalPage = PhysicalAddress + Index * EFI_PAGE_SIZE;
            UINT64 *PageTable;
            Status = EnsurePhysicalPageTable(PhysicalPage, &PageTable);
            if (EFI_ERROR(Status)) {
                return Status;
            }
            UINTN PageIndex = (UINTN)((PhysicalPage >> 12) & 0x1FF);
            if ((PageTable[PageIndex] & PAGE_TABLE_ENTRY_PRESENT) == 0) {
                PageTable[PageIndex] =
                    (PhysicalPage & PAGE_TABLE_ADDRESS_MASK) |
                    PAGE_TABLE_ENTRY_PRESENT |
                    PAGE_TABLE_ENTRY_WRITABLE |
                    PAGE_TABLE_ENTRY_NO_EXECUTE;
            }
        }
        return EFI_SUCCESS;
    }

    Level2[Level2Index] =
        (PhysicalAddress & PAGE_TABLE_ADDRESS_MASK) |
        PAGE_TABLE_ENTRY_PRESENT |
        PAGE_TABLE_ENTRY_WRITABLE |
        PAGE_TABLE_ENTRY_HUGE |
        PAGE_TABLE_ENTRY_NO_EXECUTE;
    return EFI_SUCCESS;
}

static EFI_STATUS MapPhysicalMemoryRange(UINT64 Start, UINT64 PageCount)
{
    UINT64 Size;
    UINT64 End;
    UINT64 Address;

    if (PageCount > (~0ULL / EFI_PAGE_SIZE)) {
        return EFI_INVALID_PARAMETER;
    }
    Size = PageCount * EFI_PAGE_SIZE;
    if (Start > (~0ULL - Size)) {
        return EFI_INVALID_PARAMETER;
    }
    End = Start + Size;
    if (End > PHYSICAL_MAP_WINDOW_SIZE) {
        return EFI_UNSUPPORTED;
    }

    Address = Start;
    while (Address < End) {
        EFI_STATUS Status;
        if ((Address & (HUGE_PAGE_SIZE - 1)) == 0
            && End - Address >= HUGE_PAGE_SIZE) {
            Status = MapPhysicalPage2MiB(Address);
            if (EFI_ERROR(Status)) {
                return Status;
            }
            Address += HUGE_PAGE_SIZE;
        } else {
            Status = MapPhysicalPage4KiB(Address);
            if (EFI_ERROR(Status)) {
                return Status;
            }
            Address += EFI_PAGE_SIZE;
        }
    }

    return EFI_SUCCESS;
}

static EFI_STATUS BuildPhysicalMemoryMapping(void)
{
    EFI_STATUS Status;
    UINTN MapSize = 0;
    UINTN MapKey = 0;
    UINTN DescriptorSize = 0;
    UINT32 DescriptorVersion = 0;
    EFI_MEMORY_DESCRIPTOR *Map = NULL;
    UINT64 *Level4 = (UINT64 *)(UINTN)gKernelPageTableRoot;
    UINTN DescriptorCount;

    for (gPhysicalMemoryPml4Index = 256;
         gPhysicalMemoryPml4Index < PAGE_TABLE_ENTRY_COUNT;
         gPhysicalMemoryPml4Index++) {
        if ((Level4[gPhysicalMemoryPml4Index] & PAGE_TABLE_ENTRY_PRESENT) == 0) {
            break;
        }
    }
    if (gPhysicalMemoryPml4Index == PAGE_TABLE_ENTRY_COUNT) {
        return EFI_OUT_OF_RESOURCES;
    }
    gPhysicalMemoryOffset =
        0xFFFF000000000000ULL | ((UINT64)gPhysicalMemoryPml4Index << 39);

    Status = gBS->GetMemoryMap(
        &MapSize,
        NULL,
        &MapKey,
        &DescriptorSize,
        &DescriptorVersion
    );
    if (Status != EFI_BUFFER_TOO_SMALL || DescriptorSize == 0) {
        return EFI_LOAD_ERROR;
    }

    MapSize += DescriptorSize * 32;
    for (UINTN Attempt = 0; Attempt < 8; Attempt++) {
        Status = gBS->AllocatePool(EfiLoaderData, MapSize, (VOID **)&Map);
        if (EFI_ERROR(Status)) {
            return Status;
        }

        UINTN CurrentSize = MapSize;
        Status = gBS->GetMemoryMap(
            &CurrentSize,
            Map,
            &MapKey,
            &DescriptorSize,
            &DescriptorVersion
        );
        if (Status == EFI_BUFFER_TOO_SMALL) {
            gBS->FreePool(Map);
            Map = NULL;
            MapSize = CurrentSize + DescriptorSize * 32;
            continue;
        }
        if (EFI_ERROR(Status)) {
            gBS->FreePool(Map);
            return Status;
        }

        DescriptorCount = CurrentSize / DescriptorSize;
        for (UINTN Index = 0; Index < DescriptorCount; Index++) {
            EFI_MEMORY_DESCRIPTOR *Descriptor =
                (EFI_MEMORY_DESCRIPTOR *)((UINT8 *)Map + Index * DescriptorSize);
            if (!IsDirectMappedRamType(Descriptor->Type)) {
                continue;
            }

            Status = MapPhysicalMemoryRange(
                Descriptor->PhysicalStart,
                Descriptor->NumberOfPages
            );
            if (EFI_ERROR(Status)) {
                gBS->FreePool(Map);
                return Status;
            }
        }

        gBS->FreePool(Map);
        return EFI_SUCCESS;
    }

    return EFI_OUT_OF_RESOURCES;
}

static EFI_STATUS CloneActivePageTables(EFI_PHYSICAL_ADDRESS *NewRoot)
{
    EFI_STATUS Status;
    UINT64 Cr3Value;
    UINT64 Cr4Value;

#if (defined(__GNUC__) || defined(__clang__)) && defined(__x86_64__)
    __asm__ volatile("mov %%cr3, %0" : "=r"(Cr3Value));
    __asm__ volatile("mov %%cr4, %0" : "=r"(Cr4Value));
#else
    return EFI_UNSUPPORTED;
#endif

    /* This kernel and its page-table library currently use four-level paging. */
    if ((Cr4Value & (1ULL << 12)) != 0) {
        return EFI_UNSUPPORTED;
    }

    gOriginalPageTableRoot = Cr3Value & PAGE_TABLE_ADDRESS_MASK;
    Status = gBS->AllocatePages(
        EFI_ALLOCATE_ANY_PAGES,
        EfiLoaderData,
        1,
        &gKernelPageTableRoot
    );
    if (EFI_ERROR(Status)) {
        return Status;
    }

    Status = ClonePageTableLevel(
        gOriginalPageTableRoot,
        gKernelPageTableRoot,
        4
    );
    if (EFI_ERROR(Status)) {
        return Status;
    }

    Status = BuildPhysicalMemoryMapping();
    if (EFI_ERROR(Status)) {
        return Status;
    }

    *NewRoot = gKernelPageTableRoot;
    return EFI_SUCCESS;
}

static UINTN AsciiToUcs2(const char *Ascii, CHAR16 *Ucs2, UINTN Max)
{
    UINTN i = 0;

    if (!Ascii || !Ucs2 || Max == 0) {
        return 0;
    }

    while (Ascii[i] != '\0' && i < Max - 1) {
        Ucs2[i] = (CHAR16)(unsigned char)Ascii[i];
        i++;
    }

    Ucs2[i] = 0;
    return i;
}

static VOID PrintAscii(const char *Str)
{
    CHAR16 Buffer[256];

    AsciiToUcs2(Str, Buffer, sizeof(Buffer) / sizeof(Buffer[0]));
    gST->ConOut->OutputString(gST->ConOut, Buffer);
}

static VOID PrintHex64(UINT64 Value)
{
    CHAR16 Buffer[19];
    static const char Hex[] = "0123456789ABCDEF";

    Buffer[0] = (CHAR16)'0';
    Buffer[1] = (CHAR16)'x';

    for (UINTN i = 0; i < 16; i++) {
        UINTN Shift = (15 - i) * 4;
        Buffer[2 + i] = (CHAR16)Hex[(Value >> Shift) & 0xF];
    }

    Buffer[18] = 0;
    gST->ConOut->OutputString(gST->ConOut, Buffer);
}

static const char *MemTypeName(UINT32 Type)
{
    switch (Type) {
        case EfiReservedMemoryType:      return "Reserved";
        case EfiLoaderCode:              return "LoaderCode";
        case EfiLoaderData:              return "LoaderData";
        case EfiBootServicesCode:        return "BootServicesCode";
        case EfiBootServicesData:        return "BootServicesData";
        case EfiRuntimeServicesCode:     return "RuntimeServicesCode";
        case EfiRuntimeServicesData:     return "RuntimeServicesData";
        case EfiConventionalMemory:      return "ConventionalMemory(free)";
        case EfiUnusableMemory:          return "UnusableMemory";
        case EfiACPIReclaimMemory:       return "ACPIReclaimMemory";
        case EfiACPIMemoryNVS:           return "ACPIMemoryNVS";
        case EfiMemoryMappedIO:          return "MemoryMappedIO";
        case EfiMemoryMappedIOPortSpace: return "MemoryMappedIOPortSpace";
        case EfiPalCode:                 return "PalCode";
        case EfiPersistentMemory:        return "PersistentMemory";
        default:                         return "Unknown";
    }
}

/*
 * Diagnostic only: walk the current firmware memory map and report
 * whatever descriptor covers Addr. Used when a fixed-address
 * AllocatePages call fails, so we know WHY instead of just that it did.
 */
static VOID DumpMemoryTypeAt(EFI_PHYSICAL_ADDRESS Addr)
{
    EFI_STATUS Status;
    UINTN MapSize = 0;
    UINTN MapKey = 0;
    UINTN DescriptorSize = 0;
    UINT32 DescriptorVersion = 0;
    EFI_MEMORY_DESCRIPTOR *Map = NULL;
    UINTN NumEntries;
    UINTN i;
    BOOLEAN Found = FALSE;

    Status = gBS->GetMemoryMap(&MapSize, NULL, &MapKey, &DescriptorSize, &DescriptorVersion);
    if (Status != EFI_BUFFER_TOO_SMALL) {
        return;
    }

    if (DescriptorSize == 0) {
        DescriptorSize = sizeof(EFI_MEMORY_DESCRIPTOR);
    }

    MapSize += DescriptorSize * 8;

    Status = gBS->AllocatePool(EfiLoaderData, MapSize, (VOID **)&Map);
    if (EFI_ERROR(Status)) {
        return;
    }

    Status = gBS->GetMemoryMap(&MapSize, Map, &MapKey, &DescriptorSize, &DescriptorVersion);
    if (EFI_ERROR(Status)) {
        gBS->FreePool(Map);
        return;
    }

    NumEntries = MapSize / DescriptorSize;

    PrintAscii("[INFO] Memory map entry covering 0x200000:\r\n");

    for (i = 0; i < NumEntries; i++) {
        EFI_MEMORY_DESCRIPTOR *Desc = (EFI_MEMORY_DESCRIPTOR *)((UINT8 *)Map + i * DescriptorSize);
        EFI_PHYSICAL_ADDRESS Start = Desc->PhysicalStart;
        EFI_PHYSICAL_ADDRESS End = Start + (Desc->NumberOfPages * EFI_PAGE_SIZE);

        if (Addr >= Start && Addr < End) {
            PrintAscii("       start=");
            PrintHex64(Start);
            PrintAscii(" pages=");
            PrintHex64(Desc->NumberOfPages);
            PrintAscii(" type=");
            PrintAscii(MemTypeName(Desc->Type));
            PrintAscii("\r\n");
            Found = TRUE;
            break;
        }
    }

    if (!Found) {
        PrintAscii("       No entry covers this address -- likely not installed RAM\r\n");
    }

    gBS->FreePool(Map);
}

static EFI_STATUS GetLoadedImage(EFI_LOADED_IMAGE_PROTOCOL **LoadedImage)
{
    EFI_GUID Guid = EFI_LOADED_IMAGE_PROTOCOL_GUID;

    return gBS->HandleProtocol(
        gImageHandle,
        &Guid,
        (VOID **)LoadedImage
    );
}

/* ========================================================================== */
/*                        GRAPHICS OUTPUT PROTOCOL (GOP)                      */
/* ========================================================================== */

/*
 * Must be called BEFORE ExitBootServices -- LocateProtocol is a boot
 * service. Once we've read FrameBufferBase/Size/Info out of the protocol,
 * the kernel can write straight to that physical memory after boot
 * services are gone; it never needs to call back into GOP itself.
 *
 * Not fatal if this fails (e.g. no GPU driver installed under a given
 * QEMU display backend) -- we just leave Info->FramebufferBase at 0 and
 * print a warning so the kernel can fall back to a no-graphics path.
 */
static VOID GetFramebufferInfo(BootInfo *Info)
{
    EFI_STATUS Status;
    EFI_GUID GopGuid = EFI_GRAPHICS_OUTPUT_PROTOCOL_GUID;
    EFI_GRAPHICS_OUTPUT_PROTOCOL *Gop = NULL;

    Status = gBS->LocateProtocol(&GopGuid, NULL, (VOID **)&Gop);

    if (EFI_ERROR(Status) || !Gop || !Gop->Mode || !Gop->Mode->Info) {
        PrintAscii("[WARN] Graphics Output Protocol not available -- no framebuffer\r\n");
        return;
    }

    Info->FramebufferBase             = (UINT64)Gop->Mode->FrameBufferBase;
    Info->FramebufferSize             = (UINT64)Gop->Mode->FrameBufferSize;
    Info->FramebufferWidth            = Gop->Mode->Info->HorizontalResolution;
    Info->FramebufferHeight           = Gop->Mode->Info->VerticalResolution;
    Info->FramebufferPixelsPerScanLine = Gop->Mode->Info->PixelsPerScanLine;
    Info->FramebufferPixelFormat      = (UINT32)Gop->Mode->Info->PixelFormat;

    PrintAscii("[OK] Framebuffer base=");
    PrintHex64(Info->FramebufferBase);
    PrintAscii(" size=");
    PrintHex64(Info->FramebufferSize);
    PrintAscii("\r\n");
}

/* ========================================================================== */
/*                             KERNEL FILE LOADER                             */
/* ========================================================================== */

static EFI_STATUS OpenKernelFile(
    EFI_FILE_PROTOCOL *Root,
    EFI_FILE_PROTOCOL **File
)
{
    static const char *Paths[] = {
        "\\EFI\\BOOT\\kernel.bin",
        "\\EFI\\BOOT\\KERNEL.BIN",
        "\\kernel.bin",
        "\\KERNEL.BIN",
    };

    EFI_STATUS Status = EFI_NOT_FOUND;
    CHAR16 Path[64];

    for (UINTN i = 0; i < sizeof(Paths) / sizeof(Paths[0]); i++) {
        AsciiToUcs2(Paths[i], Path, sizeof(Path) / sizeof(Path[0]));

        Status = Root->Open(
            Root,
            File,
            Path,
            EFI_FILE_MODE_READ,
            0
        );

        if (!EFI_ERROR(Status)) {
            return Status;
        }
    }

    return Status;
}

static EFI_STATUS FindBootFileSystem(EFI_SIMPLE_FILE_SYSTEM_PROTOCOL **OutFs)
{
    EFI_STATUS Status;
    EFI_GUID LoadedImageGuid = EFI_LOADED_IMAGE_PROTOCOL_GUID;
    EFI_GUID FileSystemGuid = EFI_SIMPLE_FILE_SYSTEM_PROTOCOL_GUID;
    EFI_LOADED_IMAGE_PROTOCOL *LoadedImage = NULL;

    /* 1. Získáme informace o aktuálně spuštěném binárním obrazu (vašem bootloaderu) */
    Status = gBS->HandleProtocol(
        gImageHandle,
        &LoadedImageGuid,
        (VOID **)&LoadedImage
    );
    if (EFI_ERROR(Status) || !LoadedImage) {
        PrintAscii("[ERR] Failed to open LoadedImage protocol\r\n");
        return Status;
    }

    /* 2. Zkontrolujeme, že obraz ví, z jakého zařízení (disku) byl spuštěn */
    if (!LoadedImage->DeviceHandle) {
        PrintAscii("[ERR] LoadedImage has no DeviceHandle\r\n");
        return EFI_NOT_FOUND;
    }

    /* 3. Otevřeme souborový systém přímo na tomto konkrétním bootovacím zařízení */
    Status = gBS->HandleProtocol(
        LoadedImage->DeviceHandle,
        &FileSystemGuid,
        (VOID **)OutFs
    );

    if (!EFI_ERROR(Status)) {
        return EFI_SUCCESS;
    }

    /*
     * Fallback: the boot device handle itself does not expose
     * SimpleFileSystem directly. This happens on some OVMF/QEMU builds
     * (and some real firmware) where the FS protocol is bound to a
     * different handle than the one LoadedImage->DeviceHandle points to.
     *
     * Search every handle in the system that supports
     * SimpleFileSystem and just take the first one. Fine for a
     * single-ESP-partition setup like this one.
     */
    {
        UINTN HandleCount = 0;
        EFI_HANDLE *Handles = NULL;

        Status = gBS->LocateHandleBuffer(
            ByProtocol,
            &FileSystemGuid,
            NULL,
            &HandleCount,
            &Handles
        );

        if (EFI_ERROR(Status) || HandleCount == 0) {
            PrintAscii("[ERR] Boot device does not support SimpleFileSystem protocol\r\n");
            return EFI_ERROR(Status) ? Status : EFI_NOT_FOUND;
        }

        Status = gBS->HandleProtocol(
            Handles[0],
            &FileSystemGuid,
            (VOID **)OutFs
        );

        gBS->FreePool(Handles);

        if (EFI_ERROR(Status)) {
            PrintAscii("[ERR] Boot device does not support SimpleFileSystem protocol\r\n");
            return Status;
        }
    }

    return EFI_SUCCESS;
}

static EFI_STATUS LoadKernelAt2M(UINT64 *OutFileSize)
{
    EFI_STATUS Status;

    EFI_SIMPLE_FILE_SYSTEM_PROTOCOL *FileSystem = NULL;
    EFI_FILE_PROTOCOL *Root = NULL;
    EFI_FILE_PROTOCOL *File = NULL;
    EFI_FILE_INFO *Info = NULL;

    EFI_GUID FileInfoGuid = EFI_FILE_INFO_GUID;

    UINTN InfoSize = 0;
    UINT64 FileSize = 0;
    UINTN ReadSize = 0;
    BOOLEAN PagesAllocated = FALSE;

    /* ---- FIXED: Find filesystem by searching all handles ---- */
    Status = FindBootFileSystem(&FileSystem);
    if (EFI_ERROR(Status)) {
        return Status;
    }
    /* --------------------------------------------------------- */

    Status = FileSystem->OpenVolume(FileSystem, &Root);
    if (EFI_ERROR(Status)) {
        PrintAscii("[ERR] Failed to open filesystem volume\r\n");
        return Status;
    }

    Status = OpenKernelFile(Root, &File);
    if (EFI_ERROR(Status)) {
        PrintAscii("[ERR] kernel.bin not found\r\n");
        goto Cleanup;
    }

    /*
     * Query FileInfo size.
     */
    Status = File->GetInfo(File, &FileInfoGuid, &InfoSize, NULL);
    if (Status != EFI_BUFFER_TOO_SMALL) {
        PrintAscii("[ERR] Failed to query file info size\r\n");
        goto Cleanup;
    }

    Status = gBS->AllocatePool(
        EfiBootServicesData,
        InfoSize,
        (VOID **)&Info
    );

    if (EFI_ERROR(Status)) {
        PrintAscii("[ERR] Failed to allocate file info\r\n");
        goto Cleanup;
    }

    Status = File->GetInfo(File, &FileInfoGuid, &InfoSize, Info);
    if (EFI_ERROR(Status)) {
        PrintAscii("[ERR] Failed to get file info\r\n");
        goto Cleanup;
    }

    FileSize = Info->FileSize;

    if (FileSize == 0) {
        PrintAscii("[ERR] Kernel file is empty\r\n");
        Status = EFI_LOAD_ERROR;
        goto Cleanup;
    }

    if (FileSize > (UINT64)KERNEL_RESERVE_PAGES * EFI_PAGE_SIZE) {
        PrintAscii("[ERR] Kernel too large for reserved area\r\n");
        PrintAscii("       Increase KERNEL_RESERVE_PAGES\r\n");
        Status = EFI_BAD_BUFFER_SIZE;
        goto Cleanup;
    }

    /*
     * Reserve the kernel area at 2 MiB.
     *
     * We reserve a fixed area so that .bss can be zero-initialized,
     * even though the raw kernel.bin file does not contain .bss.
     */
    {
        EFI_PHYSICAL_ADDRESS Address = KERNEL_BASE_ADDRESS;

        Status = gBS->AllocatePages(
            EFI_ALLOCATE_ADDRESS,
            EfiLoaderCode,
            KERNEL_RESERVE_PAGES,
            &Address
        );

        if (EFI_ERROR(Status)) {
            PrintAscii("[ERR] Failed to reserve 2MiB kernel area\r\n");
            PrintAscii("       Status=");
            PrintHex64(Status);
            PrintAscii("\r\n");
            DumpMemoryTypeAt(KERNEL_BASE_ADDRESS);
            goto Cleanup;
        }

        if (Address != KERNEL_BASE_ADDRESS) {
            PrintAscii("[ERR] Allocator did not give us 0x200000\r\n");
            gBS->FreePages(Address, KERNEL_RESERVE_PAGES);
            Status = EFI_LOAD_ERROR;
            goto Cleanup;
        }

        PagesAllocated = TRUE;
    }

    /*
     * Zero reserved kernel area.
     */
    ZeroMemory(
        (volatile VOID *)(UINTN)KERNEL_BASE_ADDRESS,
        (UINTN)(KERNEL_RESERVE_PAGES * EFI_PAGE_SIZE)
    );

    /*
     * Read kernel file directly to 2 MiB.
     */
    ReadSize = (UINTN)FileSize;

    Status = File->Read(
        File,
        &ReadSize,
        (VOID *)(UINTN)KERNEL_BASE_ADDRESS
    );

    if (EFI_ERROR(Status)) {
        PrintAscii("[ERR] Failed to read kernel file\r\n");
        goto Cleanup;
    }

    if (ReadSize != (UINTN)FileSize) {
        PrintAscii("[ERR] Short read while loading kernel\r\n");
        Status = EFI_LOAD_ERROR;
        goto Cleanup;
    }

    *OutFileSize = FileSize;
    Status = EFI_SUCCESS;

Cleanup:
    if (Info) {
        gBS->FreePool(Info);
    }

    if (File) {
        File->Close(File);
    }

    if (Root) {
        Root->Close(Root);
    }

    if (EFI_ERROR(Status) && PagesAllocated) {
        gBS->FreePages(KERNEL_BASE_ADDRESS, KERNEL_RESERVE_PAGES);
    }

    return Status;
}

static EFI_STATUS LoadInitrd(BootInfo *Info)
{
    EFI_STATUS Status;
    EFI_SIMPLE_FILE_SYSTEM_PROTOCOL *FileSystem = NULL;
    EFI_FILE_PROTOCOL *Root = NULL;
    EFI_FILE_PROTOCOL *File = NULL;
    EFI_FILE_INFO *FileInfo = NULL;
    EFI_GUID FileInfoGuid = EFI_FILE_INFO_GUID;
    static const CHAR16 Path[] = {
        '\\', 'E', 'F', 'I', '\\', 'B', 'O', 'O', 'T', '\\',
        'i', 'n', 'i', 't', 'r', 'd', '.', 't', 'a', 'r', 0
    };
    UINTN FileInfoSize = 0;
    UINTN ReadSize;

    Status = FindBootFileSystem(&FileSystem);
    if (EFI_ERROR(Status)) {
        return Status;
    }
    Status = FileSystem->OpenVolume(FileSystem, &Root);
    if (EFI_ERROR(Status)) {
        return Status;
    }
    Status = Root->Open(Root, &File, Path, EFI_FILE_MODE_READ, 0);
    if (EFI_ERROR(Status)) {
        goto Cleanup;
    }
    Status = File->GetInfo(File, &FileInfoGuid, &FileInfoSize, NULL);
    if (Status != EFI_BUFFER_TOO_SMALL) {
        Status = EFI_LOAD_ERROR;
        goto Cleanup;
    }
    Status = gBS->AllocatePool(EfiBootServicesData, FileInfoSize, (VOID **)&FileInfo);
    if (EFI_ERROR(Status)) {
        goto Cleanup;
    }
    Status = File->GetInfo(File, &FileInfoGuid, &FileInfoSize, FileInfo);
    if (EFI_ERROR(Status)) {
        goto Cleanup;
    }
    if (FileInfo->FileSize == 0 || FileInfo->FileSize > INITRD_MAX_SIZE) {
        Status = EFI_BAD_BUFFER_SIZE;
        goto Cleanup;
    }

    Info->InitrdSize = (UINTN)FileInfo->FileSize;
    Status = gBS->AllocatePool(EfiLoaderData, Info->InitrdSize, &Info->InitrdBase);
    if (EFI_ERROR(Status)) {
        Info->InitrdSize = 0;
        goto Cleanup;
    }
    ReadSize = Info->InitrdSize;
    Status = File->Read(File, &ReadSize, Info->InitrdBase);
    if (EFI_ERROR(Status) || ReadSize != Info->InitrdSize) {
        gBS->FreePool(Info->InitrdBase);
        Info->InitrdBase = NULL;
        Info->InitrdSize = 0;
        Status = EFI_LOAD_ERROR;
        goto Cleanup;
    }
    Status = EFI_SUCCESS;

Cleanup:
    if (FileInfo) {
        gBS->FreePool(FileInfo);
    }
    if (File) {
        File->Close(File);
    }
    if (Root) {
        Root->Close(Root);
    }
    return Status;
}

/* ========================================================================== */
/*                        FINAL MEMORY MAP / EXIT BS                          */
/* ========================================================================== */

static EFI_STATUS FinalExitBootServices(BootInfo *Info)
{
    EFI_STATUS Status;

    UINTN MapSize = 0;
    UINTN MapKey = 0;
    UINTN DescriptorSize = 0;
    UINT32 DescriptorVersion = 0;

    EFI_MEMORY_DESCRIPTOR *Map = NULL;

    UINTN Tries = 0;

    /*
     * Query required memory map size.
     */
    Status = gBS->GetMemoryMap(
        &MapSize,
        NULL,
        &MapKey,
        &DescriptorSize,
        &DescriptorVersion
    );

    if (Status != EFI_BUFFER_TOO_SMALL) {
        return EFI_LOAD_ERROR;
    }

    if (DescriptorSize == 0) {
        DescriptorSize = sizeof(EFI_MEMORY_DESCRIPTOR);
    }

    /*
     * Allocate extra room because GetMemoryMap itself can slightly change
     * the memory map.
     */
    MapSize += DescriptorSize * 32;

    while (Tries++ < 8) {
        if (!Map) {
            Status = gBS->AllocatePool(
                EfiLoaderData,
                MapSize,
                (VOID **)&Map
            );

            if (EFI_ERROR(Status)) {
                return Status;
            }
        }

        UINTN CurrentSize = MapSize;

        Status = gBS->GetMemoryMap(
            &CurrentSize,
            Map,
            &MapKey,
            &DescriptorSize,
            &DescriptorVersion
        );

        if (Status == EFI_BUFFER_TOO_SMALL) {
            gBS->FreePool(Map);
            Map = NULL;
            MapSize = CurrentSize + DescriptorSize * 32;
            continue;
        }

        if (EFI_ERROR(Status)) {
            gBS->FreePool(Map);
            return Status;
        }

        /*
         * Store final memory map info for the Rust kernel.
         */
        Info->MemoryMap = Map;
        Info->MemoryMapSize = CurrentSize;
        Info->DescriptorSize = DescriptorSize;
        Info->DescriptorVersion = DescriptorVersion;

        /*
         * Exit boot services using the MapKey from the map we just got.
         */
        Status = gBS->ExitBootServices(gImageHandle, MapKey);

        if (!EFI_ERROR(Status)) {
            return EFI_SUCCESS;
        }

        /*
         * ExitBootServices failed. Boot services are still active.
         * Retry with a fresh memory map.
         */
        if (CurrentSize > MapSize) {
            gBS->FreePool(Map);
            Map = NULL;
            MapSize = CurrentSize + DescriptorSize * 32;
        }
    }

    if (Map) {
        gBS->FreePool(Map);
    }

    return EFI_LOAD_ERROR;
}

/* ========================================================================== */
/*                                 ENTRY POINT                                */
/* ========================================================================== */

EFI_STATUS EFIAPI efi_main(
    EFI_HANDLE ImageHandle,
    EFI_SYSTEM_TABLE *SystemTable
)
{
    EFI_STATUS Status;
    UINT64 KernelFileSize = 0;
    BootInfo *Info = NULL;

    gST = SystemTable;
    gBS = SystemTable->BootServices;
    gImageHandle = ImageHandle;

    if (!gST || !gBS || !gST->ConOut) {
        return EFI_LOAD_ERROR;
    }

    gST->ConOut->ClearScreen(gST->ConOut);
    gST->ConOut->SetAttribute(
        gST->ConOut,
        EFI_TEXT_ATTR(EFI_GREEN, EFI_BLACK)
    );

    PrintAscii("LeafOS UEFI boot [ok]\r\n");
    PrintAscii("Kernel target address: 0x200000\r\n\r\n");

    /*
     * Load kernel.bin to 2 MiB.
     */
    Status = LoadKernelAt2M(&KernelFileSize);
    if (EFI_ERROR(Status)) {
        PrintAscii("[FATAL] Failed to load kernel\r\n");
        PrintAscii("Status=");
        PrintHex64(Status);
        PrintAscii("\r\n");
        return Status;
    }

    PrintAscii("[OK] Loaded kernel.bin to 0x200000\r\n");

    /*
     * Allocate boot info for Rust kernel.
     */
    Status = gBS->AllocatePool(
        EfiLoaderData,
        sizeof(BootInfo),
        (VOID **)&Info
    );

    if (EFI_ERROR(Status)) {
        PrintAscii("[FATAL] Failed to allocate BootInfo\r\n");
        gBS->FreePages(KERNEL_BASE_ADDRESS, KERNEL_RESERVE_PAGES);
        return Status;
    }

    ZeroMemory((volatile VOID *)Info, sizeof(*Info));

    Info->Magic = BOOT_INFO_MAGIC;
    Info->Version = 2;
    Info->SystemTable = SystemTable;
    Info->KernelBase = KERNEL_BASE_ADDRESS;
    Info->KernelFileSize = KernelFileSize;
    Info->KernelReservedSize = (UINT64)KERNEL_RESERVE_PAGES * EFI_PAGE_SIZE;

    Status = LoadInitrd(Info);
    if (EFI_ERROR(Status)) {
        PrintAscii("[FATAL] Failed to load initrd.tar\r\n");
        PrintAscii("Status=");
        PrintHex64(Status);
        PrintAscii("\r\n");
        return Status;
    }
    PrintAscii("[OK] Loaded read-only initrd\r\n");

    /*
     * Query the framebuffer while Boot Services (LocateProtocol) are
     * still available. Must happen before FinalExitBootServices below.
     */
    GetFramebufferInfo(Info);

    Status = CloneActivePageTables(&Info->PageTableRoot);
    if (EFI_ERROR(Status)) {
        PrintAscii("[FATAL] Failed to clone active page tables\r\n");
        PrintAscii("Status=");
        PrintHex64(Status);
        PrintAscii("\r\n");
        return Status;
    }
    Info->PhysicalMemoryOffset = gPhysicalMemoryOffset;
    PrintAscii("[OK] Cloned active page tables to ");
    PrintHex64(Info->PageTableRoot);
    PrintAscii("\r\n");
    PrintAscii("[OK] Direct-mapped RAM at ");
    PrintHex64(Info->PhysicalMemoryOffset);
    PrintAscii("\r\n");

    PrintAscii("[OK] BootInfo prepared\r\n");
    PrintAscii("[..] Exiting UEFI boot services\r\n");

    /*
     * Get final memory map and exit boot services.
     *
     * After this succeeds, UEFI Boot Services are gone.
     * No more AllocatePool, file IO, console printing, etc.
     */
    Status = FinalExitBootServices(Info);

    if (EFI_ERROR(Status)) {
        PrintAscii("[FATAL] ExitBootServices failed\r\n");
        PrintAscii("Status=");
        PrintHex64(Status);
        PrintAscii("\r\n");

        gBS->FreePages(KERNEL_BASE_ADDRESS, KERNEL_RESERVE_PAGES);
        gBS->FreePool(Info);

        return Status;
    }

    /*
     * Enter Rust kernel.
     *
     * Rust entry should be:
     *
     * #[no_mangle]
     * pub extern "C" fn _start(boot_info: *const BootInfo) -> !
     */
    typedef VOID (KERNEL_CALL_ABI *KernelEntry)(BootInfo *Info);
    KernelEntry Entry = (KernelEntry)(UINTN)KERNEL_BASE_ADDRESS;

#if (defined(__GNUC__) || defined(__clang__)) && defined(__x86_64__)
    /*
     * Start kernel with interrupts disabled and direction flag cleared.
     */
    __asm__ volatile("cli");
    __asm__ volatile("cld");
    __asm__ volatile("mov %0, %%cr3" : : "r"(Info->PageTableRoot) : "memory");
#endif

    Entry(Info);

    /*
     * Kernel should never return.
     *
     * If it does, boot services are already exited, so we cannot print
     * or recover safely. Just halt.
     */
    for (;;) {
#if (defined(__GNUC__) || defined(__clang__)) && defined(__x86_64__)
        __asm__ volatile("hlt");
#endif
    }

    return EFI_SUCCESS;
}