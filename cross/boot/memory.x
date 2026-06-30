/* Configure memory for your microcontroller bootloader */

MEMORY {
    FLASH  				(rx)  	: ORIGIN = /* 0xXXXXXXXX */, LENGTH = /* XXXK */
    BOOTLOADER_STATE 	(rx) 	: ORIGIN = /* 0xXXXXXXXX */, LENGTH = /* XXXK */
    ACTIVE 				(rx) 	: ORIGIN = /* 0xXXXXXXXX */, LENGTH = /* XXXK */
    DFU 				(rx) 	: ORIGIN = /* 0xXXXXXXXX */, LENGTH = /* XXXK */
    RAM    				(xrw) 	: ORIGIN = /* 0xXXXXXXXX */, LENGTH = /* XXXK */
}

__bootloader_state_start 	= ORIGIN(BOOTLOADER_STATE) - ORIGIN(FLASH);
__bootloader_state_end		= ORIGIN(BOOTLOADER_STATE) + LENGTH(BOOTLOADER_STATE) - ORIGIN(FLASH);

__bootloader_active_start 	= ORIGIN(ACTIVE) - ORIGIN(FLASH);
__bootloader_active_end	 	= ORIGIN(ACTIVE) + LENGTH(ACTIVE) - ORIGIN(FLASH);

__bootloader_dfu_start 		= ORIGIN(DFU) - ORIGIN(FLASH);
__bootloader_dfu_end	 	= ORIGIN(DFU) + LENGTH(DFU) - ORIGIN(FLASH);