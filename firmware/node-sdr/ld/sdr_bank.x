/*
 * node-sdr: give SRAM block 1 (0x40820000..0x4083ffff, 128 KiB) to the radio.
 *
 * The C5's dump engine can only write SRAM block 1: its owner bits take a
 * whole 128 KiB block from the CPU while it runs, block 0 holds our code and
 * data, and block 2 holds the ROM's data (esp-sdr-zephyr
 * doc/hardware-quirks.md, "The owner bit takes a whole 128 KiB block";
 * ESPARGOS esp-sdr main/targets/esp32c5/chip.h reserves 0x40820000-0x40840000
 * the same way). So nothing the CPU needs may live there:
 *
 *   0x40820000  Path B ring, 16 x 3584 = 57,344 bytes (GDMA writes it)
 *   0x4082e000  ring descriptors and small DMA-side state (8 KiB)
 *   0x40830000  capture buffer, 64 KiB: Path A's dump target (MAC_DUMP_ALLOC
 *               puts the samples 64 KiB into the block) and, after a Path B
 *               freeze, the 16,384 C16 samples handed to the decoder
 *
 * esp-hal lays RAM out as one region: .trap .rwtext .data .bss .noinit
 * .stack. This section goes right after .bss and aligns itself to the next
 * 128 KiB boundary, so code, data and .bss stay in block 0, the bank is
 * block 1, and .noinit and the main stack move to block 2. The gap between
 * the end of .bss and the bank becomes heap at run time (_sdr_gap_*), so no
 * RAM is wasted. If block 0 overflows, the bank would land on block 2 and
 * the ASSERT stops the link.
 */
SECTIONS {
  .sdr_bank (NOLOAD) : ALIGN(4)
  {
    _sdr_gap_start = ABSOLUTE(.);
    . = ALIGN(0x20000);
    _sdr_gap_end = ABSOLUTE(.);
    _sdr_bank_start = ABSOLUTE(.);
    . = . + 0x20000;
    _sdr_bank_end = ABSOLUTE(.);
  } > RAM
}
INSERT AFTER .bss;

ASSERT(_sdr_bank_start == 0x40820000,
  "node-sdr: code + data + .bss no longer fit SRAM block 0 (128 KiB); the radio bank must be block 1")
ASSERT(_sdr_bank_end == 0x40840000, "node-sdr: radio bank is not exactly SRAM block 1")
