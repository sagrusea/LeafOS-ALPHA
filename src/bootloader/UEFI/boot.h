/**
 * @file boot.h
 * @brief Core UEFI Bootloader Definitions, Types, and Protocol Abstractions
 * 
 * This header provides a self-contained set of definitions for writing
 * UEFI bootloaders without requiring the full EDK2 or GNU-EFI toolchain headers.
 * 
 * SPDX-License-Identifier: MIT OR BSD-2-Clause-Patent
 */

#ifndef BOOT_H
#define BOOT_H

#ifdef __cplusplus
extern "C" {
#endif

#include <stdint.h>
#include <stddef.h>

/* ========================================================================== */
/*                              FUNDAMENTAL TYPES                             */
/* ========================================================================== */

typedef uint8_t   UINT8;
typedef uint16_t  UINT16;
typedef uint32_t  UINT32;
typedef uint64_t  UINT64;
typedef int8_t    INT8;
typedef int16_t   INT16;
typedef int32_t   INT32;
typedef int64_t   INT64;
typedef char      CHAR8;
typedef uint16_t  CHAR16;     ///< UCS-2 character (UEFI native string format)
typedef void      VOID;
typedef uintptr_t UINTN;      ///< Native-width unsigned integer
typedef intptr_t  INTN;       ///< Native-width signed integer
typedef UINT8     BOOLEAN;
typedef UINT64    EFI_PHYSICAL_ADDRESS;
typedef UINT64    EFI_VIRTUAL_ADDRESS;
typedef UINTN     EFI_STATUS;
typedef VOID*     EFI_HANDLE;
typedef VOID*     EFI_EVENT;
typedef UINTN     EFI_TPL;

#ifndef TRUE
#define TRUE  ((BOOLEAN)1)
#endif
#ifndef FALSE
#define FALSE ((BOOLEAN)0)
#endif
#ifndef NULL
#define NULL  ((VOID *)0)
#endif

/* ========================================================================== */
/*                            EFI STATUS CODES                                */
/* ========================================================================== */

#define EFI_SUCCESS             0x0000000000000000ULL
#define EFI_LOAD_ERROR          0x8000000000000001ULL
#define EFI_INVALID_PARAMETER   0x8000000000000002ULL
#define EFI_UNSUPPORTED         0x8000000000000003ULL
#define EFI_BAD_BUFFER_SIZE     0x8000000000000004ULL
#define EFI_BUFFER_TOO_SMALL    0x8000000000000005ULL
#define EFI_NOT_FOUND           0x800000000000000EULL
#define EFI_ACCESS_DENIED       0x800000000000000FULL
#define EFI_NO_RESPONSE         0x8000000000000010ULL
#define EFI_ALREADY_STARTED     0x8000000000000014ULL
#define EFI_ABORTED             0x8000000000000015ULL
#define EFI_SECURITY_VIOLATION  0x800000000000001AULL

/// Check if an EFI_STATUS indicates an error
#define EFI_ERROR(Status) (((INTN)(Status)) < 0)

/* ========================================================================== */
/*                                 EFI GUID                                   */
/* ========================================================================== */

typedef struct {
    UINT32 Data1;
    UINT16 Data2;
    UINT16 Data3;
    UINT8  Data4[8];
} EFI_GUID;

#define EFI_GUID(a, b, c, d0, d1, d2, d3, d4, d5, d6, d7) \
    { (a), (b), (c), { (d0), (d1), (d2), (d3), (d4), (d5), (d6), (d7) } }

/* Well-Known Protocol GUIDs */
#define EFI_LOADED_IMAGE_PROTOCOL_GUID \
    EFI_GUID(0x5B1B31A1, 0x9562, 0x11D2, 0x8E, 0x3F, 0x00, 0xA0, 0xC9, 0x69, 0x72, 0x3B)

#define EFI_SIMPLE_FILE_SYSTEM_PROTOCOL_GUID \
    EFI_GUID(0x964E5B22, 0x6459, 0x11D2, 0x8E, 0x39, 0x00, 0xA0, 0xC9, 0x69, 0x72, 0x3B)

#define EFI_FILE_INFO_GUID \
    EFI_GUID(0x09576E92, 0x6D3F, 0x11D2, 0x8E, 0x39, 0x00, 0xA0, 0xC9, 0x69, 0x72, 0x3B)

#define EFI_GRAPHICS_OUTPUT_PROTOCOL_GUID \
    EFI_GUID(0x9042A9DE, 0x23DC, 0x4A38, 0x96, 0xFB, 0x7A, 0xDE, 0xD0, 0x80, 0x51, 0x6A)

#define EFI_SIMPLE_TEXT_OUTPUT_PROTOCOL_GUID \
    EFI_GUID(0x387477C2, 0x69C7, 0x11D2, 0x8E, 0x39, 0x00, 0xA0, 0xC9, 0x69, 0x72, 0x3B)

#define EFI_SIMPLE_TEXT_INPUT_PROTOCOL_GUID \
    EFI_GUID(0x387477C1, 0x69C7, 0x11D2, 0x8E, 0x39, 0x00, 0xA0, 0xC9, 0x69, 0x72, 0x3B)

#define EFI_DEVICE_PATH_PROTOCOL_GUID \
    EFI_GUID(0x09576E91, 0x6D3F, 0x11D2, 0x8E, 0x39, 0x00, 0xA0, 0xC9, 0x69, 0x72, 0x3B)

/* ========================================================================== */
/*                           MEMORY TYPES & MAP                               */
/* ========================================================================== */

typedef enum {
    EfiReservedMemoryType,
    EfiLoaderCode,
    EfiLoaderData,
    EfiBootServicesCode,
    EfiBootServicesData,
    EfiRuntimeServicesCode,
    EfiRuntimeServicesData,
    EfiConventionalMemory,
    EfiUnusableMemory,
    EfiACPIReclaimMemory,
    EfiACPIMemoryNVS,
    EfiMemoryMappedIO,
    EfiMemoryMappedIOPortSpace,
    EfiPalCode,
    EfiPersistentMemory,
    EfiMaxMemoryType
} EFI_MEMORY_TYPE;

#define EFI_ALLOCATE_ANY_PAGES      0
#define EFI_ALLOCATE_MAX_ADDRESS    1
#define EFI_ALLOCATE_ADDRESS        2

typedef UINT64 EFI_PHYSICAL_ADDRESS;

/* Memory descriptor attributes */
#define EFI_MEMORY_UC               0x0000000000000001ULL
#define EFI_MEMORY_WC               0x0000000000000002ULL
#define EFI_MEMORY_WT               0x0000000000000004ULL
#define EFI_MEMORY_WB               0x0000000000000008ULL
#define EFI_MEMORY_UCE              0x0000000000000010ULL
#define EFI_MEMORY_WP               0x0000000000001000ULL
#define EFI_MEMORY_RP               0x0000000000002000ULL
#define EFI_MEMORY_XP               0x0000000000004000ULL
#define EFI_MEMORY_NV               0x0000000000008000ULL
#define EFI_MEMORY_MORE_RELIABLE    0x0000000000010000ULL
#define EFI_MEMORY_RO               0x0000000000020000ULL
#define EFI_MEMORY_RUNTIME          0x8000000000000000ULL

typedef struct {
    UINT32                  Type;
    EFI_PHYSICAL_ADDRESS    PhysicalStart;
    EFI_VIRTUAL_ADDRESS     VirtualStart;
    UINT64                  NumberOfPages;
    UINT64                  Attribute;
} EFI_MEMORY_DESCRIPTOR;

/* ========================================================================== */
/*                          TABLE HEADER & SYSTEM TABLE                       */
/* ========================================================================== */

typedef struct {
    UINT64 Signature;
    UINT32 Revision;
    UINT32 HeaderSize;
    UINT32 CRC32;
    UINT32 Reserved;
} EFI_TABLE_HEADER;

typedef enum {
    AllHandles,
    ByRegisterNotify,
    ByProtocol
} EFI_LOCATE_SEARCH_TYPE;

/* Forward declarations for protocol interfaces */
typedef struct _EFI_SIMPLE_TEXT_OUTPUT_PROTOCOL EFI_SIMPLE_TEXT_OUTPUT_PROTOCOL;
typedef struct _EFI_SIMPLE_TEXT_INPUT_PROTOCOL  EFI_SIMPLE_TEXT_INPUT_PROTOCOL;
typedef struct _EFI_BOOT_SERVICES               EFI_BOOT_SERVICES;
typedef struct _EFI_RUNTIME_SERVICES            EFI_RUNTIME_SERVICES;
typedef struct _EFI_CONFIGURATION_TABLE         EFI_CONFIGURATION_TABLE;

typedef struct {
    EFI_TABLE_HEADER                Hdr;
    CHAR16                          *FirmwareVendor;
    UINT32                          FirmwareRevision;
    EFI_HANDLE                      ConsoleInHandle;
    EFI_SIMPLE_TEXT_INPUT_PROTOCOL  *ConIn;
    EFI_HANDLE                      ConsoleOutHandle;
    EFI_SIMPLE_TEXT_OUTPUT_PROTOCOL *ConOut;
    EFI_HANDLE                      StandardErrorHandle;
    EFI_SIMPLE_TEXT_OUTPUT_PROTOCOL *StdErr;
    EFI_RUNTIME_SERVICES            *RuntimeServices;
    EFI_BOOT_SERVICES               *BootServices;
    UINTN                           NumberOfTableEntries;
    EFI_CONFIGURATION_TABLE         *ConfigurationTable;
} EFI_SYSTEM_TABLE;

/* ========================================================================== */
/*                         KEY PROTOCOL STRUCTURES                            */
/* ========================================================================== */

/* --- Simple Text Output --- */
typedef struct {
    INT32 MaxMode;
    INT32 Mode;
    /* ... additional fields omitted for brevity */
} SIMPLE_TEXT_OUTPUT_MODE;

struct _EFI_SIMPLE_TEXT_OUTPUT_PROTOCOL {
    EFI_STATUS (*Reset)(EFI_SIMPLE_TEXT_OUTPUT_PROTOCOL *This, BOOLEAN ExtendedVerification);
    EFI_STATUS (*OutputString)(EFI_SIMPLE_TEXT_OUTPUT_PROTOCOL *This, CHAR16 *String);
    EFI_STATUS (*TestString)(EFI_SIMPLE_TEXT_OUTPUT_PROTOCOL *This, CHAR16 *String);
    EFI_STATUS (*QueryMode)(EFI_SIMPLE_TEXT_OUTPUT_PROTOCOL *This, UINTN ModeNumber, UINTN *Columns, UINTN *Rows);
    EFI_STATUS (*SetMode)(EFI_SIMPLE_TEXT_OUTPUT_PROTOCOL *This, UINTN ModeNumber);
    EFI_STATUS (*SetAttribute)(EFI_SIMPLE_TEXT_OUTPUT_PROTOCOL *This, UINTN Attribute);
    EFI_STATUS (*ClearScreen)(EFI_SIMPLE_TEXT_OUTPUT_PROTOCOL *This);
    EFI_STATUS (*SetCursorPosition)(EFI_SIMPLE_TEXT_OUTPUT_PROTOCOL *This, UINTN Column, UINTN Row);
    EFI_STATUS (*EnableCursor)(EFI_SIMPLE_TEXT_OUTPUT_PROTOCOL *This, BOOLEAN Visible);
    SIMPLE_TEXT_OUTPUT_MODE *Mode;
};

/* Text Attributes */
#define EFI_BLACK           0x00
#define EFI_BLUE            0x01
#define EFI_GREEN           0x02
#define EFI_CYAN            0x03
#define EFI_RED             0x04
#define EFI_MAGENTA         0x05
#define EFI_BROWN           0x06
#define EFI_LIGHTGRAY       0x07
#define EFI_BRIGHT          0x08
#define EFI_BACKGROUND_BLACK     0x00
#define EFI_BACKGROUND_BLUE      0x10
#define EFI_BACKGROUND_GREEN     0x20
#define EFI_BACKGROUND_CYAN      0x30
#define EFI_BACKGROUND_RED       0x40
#define EFI_BACKGROUND_MAGENTA   0x50
#define EFI_BACKGROUND_BROWN     0x60
#define EFI_BACKGROUND_LIGHTGRAY 0x70

#define EFI_TEXT_ATTR(Foreground, Background) \
    ((Foreground) | ((Background) << 4))

/* --- Graphics Output Protocol (GOP) --- */
typedef enum {
    PixelRedGreenBlueReserved8BitPerColor,
    PixelBlueGreenRedReserved8BitPerColor,
    PixelBitMask,
    PixelBltOnly,
    PixelFormatMax
} EFI_GRAPHICS_PIXEL_FORMAT;

typedef struct {
    UINT32 RedMask;
    UINT32 GreenMask;
    UINT32 BlueMask;
    UINT32 ReservedMask;
} EFI_PIXEL_BITMASK;

typedef struct {
    UINT32                     Version;
    UINT32                     HorizontalResolution;
    UINT32                     VerticalResolution;
    EFI_GRAPHICS_PIXEL_FORMAT  PixelFormat;
    EFI_PIXEL_BITMASK          PixelInformation;
    UINT32                     PixelsPerScanLine;
} EFI_GRAPHICS_OUTPUT_MODE_INFORMATION;

typedef struct {
    UINT32                                 MaxMode;
    UINT32                                 Mode;
    EFI_GRAPHICS_OUTPUT_MODE_INFORMATION   *Info;
    UINTN                                  SizeOfInfo;
    EFI_PHYSICAL_ADDRESS                   FrameBufferBase;
    UINTN                                  FrameBufferSize;
} EFI_GRAPHICS_OUTPUT_PROTOCOL_MODE;

typedef struct _EFI_GRAPHICS_OUTPUT_PROTOCOL EFI_GRAPHICS_OUTPUT_PROTOCOL;

struct _EFI_GRAPHICS_OUTPUT_PROTOCOL {
    EFI_STATUS (*QueryMode)(
        EFI_GRAPHICS_OUTPUT_PROTOCOL *This,
        UINT32 ModeNumber,
        UINTN *SizeOfInfo,
        EFI_GRAPHICS_OUTPUT_MODE_INFORMATION **Info
    );
    EFI_STATUS (*SetMode)(
        EFI_GRAPHICS_OUTPUT_PROTOCOL *This,
        UINT32 ModeNumber
    );
    /* Blt() omitted -- not needed once we write to FrameBufferBase directly */
    VOID *Blt;
    EFI_GRAPHICS_OUTPUT_PROTOCOL_MODE *Mode;
};

/* --- Loaded Image Protocol --- */
typedef struct {
    UINT32                      Revision;
    EFI_HANDLE                  ParentHandle;
    EFI_SYSTEM_TABLE            *SystemTable;
    EFI_HANDLE                  DeviceHandle;
    VOID                        *FilePath;
    VOID                        *Reserved;
    UINT32                      LoadOptionsSize;
    VOID                        *LoadOptions;
    VOID                        *ImageBase;
    UINT64                      ImageSize;
    EFI_MEMORY_TYPE             ImageCodeType;
    EFI_MEMORY_TYPE             ImageDataType;
    /* Unload function pointer omitted for simplicity */
} EFI_LOADED_IMAGE_PROTOCOL;

/* ========================================================================== */
/*                          BOOT SERVICES (Subset)                            */
/* ========================================================================== */

struct _EFI_BOOT_SERVICES {
    EFI_TABLE_HEADER Hdr;

    /* Task Priority */
    EFI_STATUS (*RaiseTPL)(EFI_TPL NewTpl);
    VOID       (*RestoreTPL)(EFI_TPL OldTpl);

    /* Memory */
    EFI_STATUS (*AllocatePages)(UINTN Type, EFI_MEMORY_TYPE MemoryType, UINTN Pages, EFI_PHYSICAL_ADDRESS *Memory);
    EFI_STATUS (*FreePages)(EFI_PHYSICAL_ADDRESS Memory, UINTN Pages);
    EFI_STATUS (*GetMemoryMap)(UINTN *MemoryMapSize, EFI_MEMORY_DESCRIPTOR *MemoryMap, UINTN *MapKey, UINTN *DescriptorSize, UINT32 *DescriptorVersion);
    EFI_STATUS (*AllocatePool)(EFI_MEMORY_TYPE PoolType, UINTN Size, VOID **Buffer);
    EFI_STATUS (*FreePool)(VOID *Buffer);

    /* Event/Timer (stubs) */
    VOID *CreateEvent;
    VOID *SetTimer;
    VOID *WaitForEvent;
    VOID *SignalEvent;
    VOID *CloseEvent;
    VOID *CheckEvent;

    /* Protocol Handling */
    EFI_STATUS (*InstallProtocolInterface)(EFI_HANDLE *Handle, EFI_GUID *Protocol, UINTN InterfaceType, VOID *Interface);
    VOID *ReinstallProtocolInterface;
    VOID *UninstallProtocolInterface;
    EFI_STATUS (*HandleProtocol)(EFI_HANDLE Handle, EFI_GUID *Protocol, VOID **Interface);
    VOID *Reserved;
    VOID *RegisterProtocolNotify;
    VOID *LocateHandle;
    VOID *LocateDevicePath;
    VOID *InstallConfigurationTable;

    /* Image Services */
    EFI_STATUS (*LoadImage)(BOOLEAN BootPolicy, EFI_HANDLE ParentImageHandle, VOID *DevicePath, VOID *SourceBuffer, UINTN SourceSize, EFI_HANDLE *ImageHandle);
    EFI_STATUS (*StartImage)(EFI_HANDLE ImageHandle, UINTN *ExitDataSize, CHAR16 **ExitData);
    VOID *Exit;
    VOID *UnloadImage;
    EFI_STATUS (*ExitBootServices)(EFI_HANDLE ImageHandle, UINTN MapKey);

    /* Misc */
    VOID *GetNextMonotonicCount;
    EFI_STATUS (*Stall)(UINTN Microseconds);
    VOID *SetWatchdogTimer;

    /* Driver Support */
    VOID *ConnectController;
    VOID *DisconnectController;

    /* Open/Close Protocol */
    VOID *OpenProtocol;
    VOID *CloseProtocol;
    VOID *OpenProtocolInformation;

    /* Library */
    VOID *ProtocolsPerHandle;
    EFI_STATUS (*LocateHandleBuffer)(
        EFI_LOCATE_SEARCH_TYPE SearchType,
        EFI_GUID *Protocol,
        VOID *SearchKey,
        UINTN *NoHandles,
        EFI_HANDLE **Buffer
    );
    EFI_STATUS (*LocateProtocol)(EFI_GUID *Protocol, VOID *Registration, VOID **Interface);
    VOID *InstallMultipleProtocolInterfaces;
    VOID *UninstallMultipleProtocolInterfaces;

    /* CRC32 */
    VOID *CalculateCrc32;

    /* Misc */
    VOID *CopyMem;
    VOID *SetMem;
    VOID *CreateEventEx;
};

/* ========================================================================== */
/*                          HELPER MACROS                                     */
/* ========================================================================== */

/**
 * @brief Print a UCS-2 string to the UEFI console
 */
#define EFI_PRINT(Str) gST->ConOut->OutputString(gST->ConOut, (CHAR16*)(Str))

/**
 * @brief Convert ASCII string literal to UCS-2 at compile time (simple cases)
 * @note  For runtime conversion, implement AsciiToUcs2(). This macro is for
 *        static string literals only.
 */
#define L(s) u##s

/**
 * @brief Entry point signature for UEFI applications/bootloaders
 */
#define EFI_ENTRYPOINT EFIAPI efi_main

#if defined(__GNUC__) || defined(__clang__)
    #define EFIAPI __attribute__((ms_abi))
#elif defined(_MSC_VER)
    #define EFIAPI __cdecl
#else
    #define EFIAPI
#endif

/** Global System Table pointer (must be defined in your .c file) */
extern EFI_SYSTEM_TABLE *gST;
extern EFI_BOOT_SERVICES *gBS;
extern EFI_HANDLE gImageHandle;

#ifdef __cplusplus
}
#endif

#endif /* EFI_BOOTLOADER_H */