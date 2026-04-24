/* Heltec Mesh Node T114 — bare-metal, no SoftDevice, direct probe-rs flash
 *   Application  0x00000 - 0xF4000  (976 KiB)
 *   (reserved)   0xF4000 - 0x100000
 */
MEMORY
{
  FLASH : ORIGIN = 0x00000000, LENGTH = 976K
  RAM   : ORIGIN = 0x20000000, LENGTH = 256K
}
