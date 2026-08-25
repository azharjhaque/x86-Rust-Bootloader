/* The kernel is loaded at a fixed low physical address that UEFI
   identity-maps, so no page-table work is needed in this milestone.
   2 MiB is above the legacy low-memory clutter and comfortably clear of
   anything OVMF places. */
ENTRY(_start)

SECTIONS {
    . = 2M;

    .text   : ALIGN(4K) { *(.text .text.*) }
    .rodata : ALIGN(4K) { *(.rodata .rodata.*) }
    .data   : ALIGN(4K) { *(.data .data.*) }
    .bss    : ALIGN(4K) { *(.bss .bss.*) *(COMMON) }

    /DISCARD/ : { *(.eh_frame) *(.note .note.*) *(.comment) }
}
