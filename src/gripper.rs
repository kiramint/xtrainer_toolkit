use crate::config::GripperConfig;
use anyhow::{ensure, Result};
use serialport::SerialPort;
use std::{
    io::{Read, Write},
    time::Duration,
};

pub struct Gripper {
    port: Box<dyn SerialPort>,
    id: u8,
}

fn checksum(data: &[u8]) -> u8 {
    !data.iter().fold(0u8, |sum, v| sum.wrapping_add(*v))
}

fn status(data: &[u8], id: u8, count: usize) -> Result<&[u8]> {
    ensure!(data.len() == count + 6, "夹爪响应长度错误");
    ensure!(
        data[..2] == [255, 255] && data[2] == id && data[3] as usize == count + 2,
        "夹爪响应头或电机编号错误"
    );
    ensure!(
        data[data.len() - 1] == checksum(&data[2..data.len() - 1]),
        "夹爪响应校验失败"
    );
    ensure!(data[4] == 0, "夹爪错误码: {}", data[4]);
    Ok(&data[5..data.len() - 1])
}

impl Gripper {
    pub fn connect(cfg: &GripperConfig) -> Result<Self> {
        ensure!(
            cfg.motor_id < 254 && !cfg.port.trim().is_empty(),
            "夹爪配置无效"
        );
        let port = serialport::new(&cfg.port, 1_000_000)
            .timeout(Duration::from_millis(100))
            .open()?;
        let mut bus = Self {
            port,
            id: cfg.motor_id,
        };
        bus.request(1, &[], 0)?;
        bus.request(2, &[3, 2], 2)?;
        Ok(bus)
    }
    fn request(&mut self, instruction: u8, params: &[u8], count: usize) -> Result<Vec<u8>> {
        let mut packet = vec![255, 255, self.id, params.len() as u8 + 2, instruction];
        packet.extend_from_slice(params);
        packet.push(checksum(&packet[2..]));
        self.port.write_all(&packet)?;
        let mut response = vec![0; count + 6];
        self.port.read_exact(&mut response)?;
        Ok(status(&response, self.id, count)?.to_vec())
    }
    pub fn position(&mut self) -> Result<u16> {
        let data = self.request(2, &[56, 2], 2)?;
        let position = u16::from_le_bytes([data[0], data[1]]);
        ensure!(position & 0x8000 == 0, "夹爪位置为负数");
        Ok(position)
    }
    pub fn write(&mut self, position: u16) -> Result<()> {
        ensure!((2048..=3052).contains(&position), "夹爪目标超出标定范围");
        let [lo, hi] = position.to_le_bytes();
        self.request(3, &[41, 0, lo, hi, 0, 0, 0, 16], 0)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_bad_status() {
        let mut data = vec![255, 255, 21, 2, 0, 232];
        assert!(status(&data, 21, 0).is_ok());
        assert!(status(&data, 22, 0).is_err());
        data[5] = 0;
        assert!(status(&data, 21, 0).is_err());
    }
}
