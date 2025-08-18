use embassy_time::Timer;
// use embedded_hal_async::spi::SpiDevice;
use embedded_hal::spi::SpiDevice;
use ergot::fmt;

use crate::STACK;

pub struct Acc<D: SpiDevice> {
    pub(crate) d: D,
}

pub mod regs {
    pub const RESERVED_00: u8 = 0x00; // 00000000 - Reserved
    pub const FUNC_CFG_ACCESS: u8 = 0x01; // 00000001 00000000 Embedded functions
    pub const RESERVED_02: u8 = 0x02; // 00000010 - Reserved
    pub const RESERVED_03: u8 = 0x03; // 00000011 - Reserved
    pub const SENSOR_SYNC_TIME_FRAME: u8 = 0x04; // 00000100 00000000 Sensor sync
    pub const SENSOR_SYNC_RES_RATIO: u8 = 0x05; // 00000101 00000000
    pub const FIFO_CTRL1: u8 = 0x06; // 00000110 00000000
    pub const FIFO_CTRL2: u8 = 0x07; // 00000111 00000000
    pub const FIFO_CTRL3: u8 = 0x08; // 00001000 00000000
    pub const FIFO_CTRL4: u8 = 0x09; // 00001001 00000000
    pub const FIFO_CTRL5: u8 = 0x0A; // 00001010 00000000
    pub const DRDY_PULSE_CFG_G: u8 = 0x0B; // 00001011 00000000
    pub const RESERVED_0C: u8 = 0x0C; // 00001100 - Reserved
    pub const INT1_CTRL: u8 = 0x0D; // 00001101 00000000 INT1 pin control
    pub const INT2_CTRL: u8 = 0x0E; // 00001110 00000000 INT2 pin control
    pub const WHO_AM_I: u8 = 0x0F; // 00001111 01101010 Who I am ID
    pub const CTRL1_XL: u8 = 0x10; // 00010000 00000000
    pub const CTRL2_G: u8 = 0x11; // 00010001 00000000
    pub const CTRL3_C: u8 = 0x12; // 00010010 00000100
    pub const CTRL4_C: u8 = 0x13; // 00010011 00000000
    pub const CTRL5_C: u8 = 0x14; // 00010100 00000000
    pub const CTRL6_C: u8 = 0x15; // 00010101 00000000
    pub const CTRL7_G: u8 = 0x16; // 00010110 00000000
    pub const CTRL8_XL: u8 = 0x17; // 0001 0111 00000000
    pub const CTRL9_XL: u8 = 0x18; // 00011000 00000000
    pub const CTRL10_C: u8 = 0x19; // 00011001 00000000
    pub const MASTER_CONFIG: u8 = 0x1A; // 00011010 00000000 I2C masterconfiguration register
    pub const WAKE_UP_SRC: u8 = 0x1B; // 00011011 output Interrupt registers
    pub const TAP_SRC: u8 = 0x1C; // 00011100 output
    pub const D6D_SRC: u8 = 0x1D; // 00011101 output
    pub const STATUS_REG: u8 = 0x1E; // 00011110 output Status data register foruser interface
    pub const RESERVED_1F: u8 = 0x1F; // 00011111 -
    pub const OUT_TEMP_L: u8 = 0x20; // 00100000 output Temperature outputdata registers
    pub const OUT_TEMP_H: u8 = 0x21; // 00100001 output
    pub const OUTX_L_G: u8 = 0x22; // 00100010 output Gyroscope output registers for user interface
    pub const OUTX_H_G: u8 = 0x23; // 00100011 output
    pub const OUTY_L_G: u8 = 0x24; // 00100100 output
    pub const OUTY_H_G: u8 = 0x25; // 00100101 output
    pub const OUTZ_L_G: u8 = 0x26; // 00100110 output
    pub const OUTZ_H_G: u8 = 0x27; // 00100111 output
    pub const OUTX_L_XL: u8 = 0x28; // 00101000 output Accelerometer output registers
    pub const OUTX_H_XL: u8 = 0x29; // 00101001 output
    pub const OUTY_L_XL: u8 = 0x2A; // 00101010 output
    pub const OUTY_H_XL: u8 = 0x2B; // 00101011 output
    pub const OUTZ_L_XL: u8 = 0x2C; // 00101100 output
    pub const OUTZ_H_XL: u8 = 0x2D; // 00101101 output
    pub const SENSORHUB1_REG: u8 = 0x2E; // 00101110 output Sensor hub output registers
    pub const SENSORHUB2_REG: u8 = 0x2F; // 00101111 output
    pub const SENSORHUB3_REG: u8 = 0x30; // 00110000 output
    pub const SENSORHUB4_REG: u8 = 0x31; // 00110001 output
    pub const SENSORHUB5_REG: u8 = 0x32; // 00110010 output
    pub const SENSORHUB6_REG: u8 = 0x33; // 00110011 output
    pub const SENSORHUB7_REG: u8 = 0x34; // 00110100 output
    pub const SENSORHUB8_REG: u8 = 0x35; // 00110101 output
    pub const SENSORHUB9_REG: u8 = 0x36; // 00110110 output
    pub const SENSORHUB10_REG: u8 = 0x37; // 00110111 output
    pub const SENSORHUB11_REG: u8 = 0x38; // 00111000 output
    pub const SENSORHUB12_REG: u8 = 0x39; // 00111001 output
    pub const FIFO_STATUS1: u8 = 0x3A; //  00111010 output FIFO status registers
    pub const FIFO_STATUS2: u8 = 0x3B; //  00111011 output
    pub const FIFO_STATUS3: u8 = 0x3C; //  00111100 output
    pub const FIFO_STATUS4: u8 = 0x3D; //  00111101 output
    pub const FIFO_DATA_OUT_L: u8 = 0x3E; //  00111110 output FIFO data output registers
    pub const FIFO_DATA_OUT_H: u8 = 0x3F; //  00111111 output
    pub const TIMESTAMP0_REG: u8 = 0x40; //  01000000 output Timestamp output registers
    pub const TIMESTAMP1_REG: u8 = 0x41; //  01000001 output
    pub const TIMESTAMP2_REG: u8 = 0x42; // 01000010 output
    pub const RESERVED_43: u8 = 0x43;
    pub const RESERVED_44: u8 = 0x44;
    pub const RESERVED_45: u8 = 0x45;
    pub const RESERVED_46: u8 = 0x46;
    pub const RESERVED_47: u8 = 0x47;
    pub const RESERVED_48: u8 = 0x48;
    pub const STEP_TIMESTAMP_L: u8 = 0x49; //  0100 1001 output Step counter timestamp registers
    pub const STEP_TIMESTAMP_H: u8 = 0x4A; //  0100 1010 output
    pub const STEP_COUNTER_L: u8 = 0x4B; //  01001011 output Step counter output registers
    pub const STEP_COUNTER_H: u8 = 0x4C; //  01001100 output
    pub const SENSORHUB13_REG: u8 = 0x4D; //  01001101 output Sensor hub output registers
    pub const SENSORHUB14_REG: u8 = 0x4E; //  01001110 output
    pub const SENSORHUB15_REG: u8 = 0x4F; //  01001111 output
    pub const SENSORHUB16_REG: u8 = 0x50; //  01010000 output
    pub const SENSORHUB17_REG: u8 = 0x51; //  01010001 output
    pub const SENSORHUB18_REG: u8 = 0x52; //  01010010 output
    pub const FUNC_SRC1: u8 = 0x53; //  01010011 output Interrupt registers
    pub const FUNC_SRC2: u8 = 0x54; //  01010100 output
    pub const WRIST_TILT_IA: u8 = 0x55; //  01010101 output Interrupt register
    pub const RESERVED_56: u8 = 0x56;
    pub const RESERVED_57: u8 = 0x57;
    pub const TAP_CFG: u8 = 0x58; // 01011000 00000000 Interrupt registers
    pub const TAP_THS_6D: u8 = 0x59; // 01011001 00000000
    pub const INT_DUR2: u8 = 0x5A; // 01011010 00000000
    pub const WAKE_UP_THS: u8 = 0x5B; // 01011011 00000000
    pub const WAKE_UP_DUR: u8 = 0x5C; // 01011100 00000000
    pub const FREE_FALL: u8 = 0x5D; // 01011101 00000000
    pub const MD1_CFG: u8 = 0x5E; // 01011110 00000000
    pub const MD2_CFG: u8 = 0x5F; // 01011111 00000000
    pub const MASTER_CMD_CODE: u8 = 0x60; // 01100000 00000000
    pub const SENS_SYNC_SPI_ERROR_CODE: u8 = 0x61; // 0110 0001 00000000
    pub const RESERVED_62: u8 = 0x62;
    pub const RESERVED_63: u8 = 0x63;
    pub const RESERVED_64: u8 = 0x64;
    pub const RESERVED_65: u8 = 0x65;
    pub const OUT_MAG_RAW_X_L: u8 = 0x66; // 01100110 output External magnetometer raw data output registers
    pub const OUT_MAG_RAW_X_H: u8 = 0x67; // 01100111 output
    pub const OUT_MAG_RAW_Y_L: u8 = 0x68; // 01101000 output
    pub const OUT_MAG_RAW_Y_H: u8 = 0x69; // 01101001 output
    pub const OUT_MAG_RAW_Z_L: u8 = 0x6A; // 01101010 output
    pub const OUT_MAG_RAW_Z_H: u8 = 0x6B; // 01101011 output
    pub const RESERVED_6C: u8 = 0x6C;
    pub const RESERVED_6D: u8 = 0x6D;
    pub const RESERVED_6E: u8 = 0x6E;
    pub const RESERVED_6F: u8 = 0x6F;
    pub const RESERVED_70: u8 = 0x70;
    pub const RESERVED_71: u8 = 0x71;
    pub const RESERVED_72: u8 = 0x72;
    pub const X_OFS_USR: u8 = 0x73; // 01110011 00000000 Accelerometer user offset correction
    pub const Y_OFS_USR: u8 = 0x74; // 01110100 00000000
    pub const Z_OFS_USR: u8 = 0x75; // 01110101 00000000
    pub const RESERVED_76: u8 = 0x76;
    pub const RESERVED_77: u8 = 0x77;
    pub const RESERVED_78: u8 = 0x78;
    pub const RESERVED_79: u8 = 0x79;
    pub const RESERVED_7A: u8 = 0x7A;
    pub const RESERVED_7B: u8 = 0x7B;
    pub const RESERVED_7C: u8 = 0x7C;
    pub const RESERVED_7D: u8 = 0x7D;
    pub const RESERVED_7E: u8 = 0x7E;
    pub const RESERVED_7F: u8 = 0x7F;
}

impl<D: SpiDevice> Acc<D> {
    pub fn read8(&mut self, reg: u8) -> Result<u8, D::Error> {
        let mut buf = [reg | 0x80, 0x00];
        self.d.transfer_in_place(&mut buf)?;
        Ok(buf[1])
    }

    pub fn write8(&mut self, reg: u8, val: u8) -> Result<(), D::Error> {
        let mut buf = [reg & !0x80, val];
        self.d.transfer_in_place(&mut buf)?;
        Ok(())
    }

    pub fn read_timestamp(&mut self) -> Result<[u8; 3], D::Error> {
        let mut buf = [0u8; 4];
        buf[0] = regs::TIMESTAMP0_REG | 0x80;
        self.d.transfer_in_place(&mut buf)?;
        let mut out = [0u8; 3];
        out.copy_from_slice(&buf[1..4]);
        Ok(out)
    }

    pub fn read_one_fifo_with_bits(&mut self) -> Result<[u8; 6], D::Error> {
        let mut buf = [0u8; 7];
        buf[0] = regs::FIFO_STATUS1 | 0x80;
        self.d.transfer_in_place(&mut buf)?;
        let mut out = [0u8; 6];
        out.copy_from_slice(&buf[1..7]);
        Ok(out)
    }

    pub async fn james_setup(&mut self) -> Result<(), D::Error> {
        loop {
            let res = self.read8(regs::WHO_AM_I);
            match res {
                Ok(g) => {
                    STACK.info_fmt(fmt!("Ok: {g:02X}"));
                    break;
                }
                Err(_e) => {
                    STACK.info_fmt(fmt!("Err :("));
                    Timer::after_secs(1).await;
                }
            }
        }

        _ = self.write8(regs::CTRL3_C, 0b0000_0001);
        Timer::after_millis(100).await;

        let steps: &[(u8, u8)] = &[
            // Data Ready is NOT pulsed, no wrist tilt interrupt
            (regs::DRDY_PULSE_CFG_G, 0b0000_0000),
            // ONLY fifo threshold INT1
            (regs::INT1_CTRL, 0b0000_1000),
            // ACC 6.66kHz (XL_HM_MODE = ?), +/-2g, bandwidth stuff off?
            // Note: CTRL6 can be used to disable high power mode
            (regs::CTRL1_XL, 0b1010_0000),
            // (regs::CTRL1_XL, 0b0001_0000), // 12.5
            // (regs::CTRL1_XL, 0b1000_0000),
            // GYR 6.66kHz (G_HM_MODE = ?). 245 dps
            // Note: CTRL7 can be used to disable high power mode
            (regs::CTRL2_G, 0b1010_0000),
            // (regs::CTRL2_G, 0b0001_0000), // 12.5
            // (regs::CTRL2_G, 0b1000_0000),
            // Retain memory content, block data update, int active low,
            // int open-drain, 4-wire SPI, increment reg access, little (?) endian,
            // don't reset
            (regs::CTRL3_C, 0b0111_0100),
            // ???, disable I2C
            (regs::CTRL4_C, 0b0000_0100),
            // Timer resolution: 25us
            (regs::WAKE_UP_DUR, 0b0001_0000),
            // Enable timer
            (regs::CTRL10_C, 0b0010_0000),
            // Reset timer
            (regs::TIMESTAMP2_REG, 0xAA),
            // Disable FIFO to reset
            (regs::FIFO_CTRL5, 0b0000_0000),
            // FTH[7..0]: 36 words/72 bytes
            (regs::FIFO_CTRL1, 0b0010_0100),
            // No pedometer, no temp, threshold upper bits zero
            (regs::FIFO_CTRL2, 0b1000_0000),
            // No decimation, gyro + acc in fifo
            (regs::FIFO_CTRL3, 0b0000_1001),
            // No stop on threshold, No high-only data, no 3rd/4th
            // data in FIFO
            (regs::FIFO_CTRL4, 0b0000_1000),
            // FIFO ODR is 6.66kHz, Mode is Continuous
            (regs::FIFO_CTRL5, 0b0101_0110),
            // (regs::FIFO_CTRL5, 0b0000_1110), // 12.5
            // (regs::FIFO_CTRL5, 0b0100_0110),
        ];

        for (addr, val) in steps.iter().copied() {
            STACK.info_fmt(fmt!("Writing to addr {addr:02X}, value {val:02X}"));
            self.write8(addr, val)?;
            Timer::after_millis(10).await;
        }

        Ok(())
    }
}

// Setup Plan

/*
FUNC_CFG_ACCESS         - Probably don't care?
SENSOR_SYNC_TIME_FRAME  - Probably don't care?
SENSOR_SYNC_RES_RATIO   - Probably don't care?
FIFO_CTRL1              X Need to write FIFO threshold
FIFO_CTRL2              X Need to write FIFO threshold and maybe fifo write mode?
FIFO_CTRL3              - Probably don't care? Decimation
FIFO_CTRL4              X Maybe? Some more Decimation stuff
FIFO_CTRL5              X Yes, FIFO output data rate stuff
DRDY_PULSE_CFG_G        X Maybe? Data Ready pulsed vs latched mode
INT1_CTRL               X Yes, probably get data ready/threshold
INT2_CTRL               - Probably don't care? Int2?
WHO_AM_I                - N/A
CTRL1_XL                X Yes, set XL data rate and stuff
CTRL2_G                 X Yes, set Gyro data rate and stuff
CTRL3_C                 X Yes, interrupt pad config we probably want, also block data update
CTRL4_C                 X Yes, disable i2c and maybe data ready stuff?
CTRL5_C                 - Probably not, self test stuff?
CTRL6_C                 - Probably not, tuning/filter things?
CTRL7_G                 - Probably not, misc gyro
CTRL8_XL                - Probably not, misc accel
CTRL9_XL                - Probably not, misc accel
CTRL10_C                - Probably not, misc stuff
MASTER_CONFIG           - Probably not, sensor hub stuff?
WAKE_UP_SRC             - Probably not, d/c sleep stuff?
TAP_SRC                 - Probably not, d/c tap stuff?
D6D_SRC                 - Probably not?
STATUS_REG              - Misc data available reg
*/
