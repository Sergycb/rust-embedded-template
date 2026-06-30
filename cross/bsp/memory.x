/* Configure memory for your microcontroller, considering bootloader size */

MEMORY {
    FLASH  (rx)  : ORIGIN = /* 0xXXXXXXXX */, LENGTH = /* XXXK */
    RAM    (xrw) : ORIGIN = /* 0xXXXXXXXX */, LENGTH = /* XXXK */
}