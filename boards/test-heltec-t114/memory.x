/* Heltec Mesh Node T114 — no SoftDevice
 *   MBR          0x00000 - 0x01000
 *   Application  0x01000 - 0xF4000  (972 KiB)
 *   Bootloader   0xF4000 - 0x100000
 */
MEMORY
{
  FLASH : ORIGIN = 0x00001000, LENGTH = 972K
  RAM   : ORIGIN = 0x20000000, LENGTH = 256K
}
