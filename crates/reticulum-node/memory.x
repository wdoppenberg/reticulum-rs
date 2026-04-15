/* nRF52840 memory layout */
MEMORY
{
  /* Flash: 1 MB */
  FLASH : ORIGIN = 0x00000000, LENGTH = 1024K
  /* RAM: 256 KB */
  RAM   : ORIGIN = 0x20000000, LENGTH = 256K
}
