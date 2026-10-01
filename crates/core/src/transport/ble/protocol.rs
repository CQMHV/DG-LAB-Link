//! 郊狼 3.0 BLE wire 编码；设备业务与队列生命周期留在 actor 中。

use crate::dglab::v4::V3WaveFrame;
use crate::model::{Channel, WaveFrame};

pub const STANDARD_MODE: [u8; 9] = [0xBC, 0x04, 0x32, 0x32, 0x32, 0x1E, 0x50, 0x96, 0x32];
pub const STANDARD_MODE_ACK: [u8; 9] = [0xBD, 0x04, 0x32, 0x32, 0x32, 0x1E, 0x50, 0x96, 0x32];

/// 无效波形意味着该通道不输出；与设备基础强度无关。
const NO_OUTPUT: [u8; 8] = [0, 0, 0, 0, 0, 0, 0, 0x65];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StrengthChange {
    Unchanged,
    Increase(u8),
    Decrease(u8),
    Zero,
}

impl StrengthChange {
    fn wire(self) -> (u8, u8) {
        match self {
            Self::Unchanged => (0, 0),
            Self::Increase(value) => (1, value),
            Self::Decrease(value) => (2, value),
            Self::Zero => (3, 0),
        }
    }
}

pub fn b0(sequence: u8, strength: [StrengthChange; 2], waves: [Option<WaveFrame>; 2]) -> [u8; 20] {
    debug_assert!(sequence <= 15);
    let (mode_a, value_a) = strength[0].wire();
    let (mode_b, value_b) = strength[1].wire();
    let mut bytes = [0; 20];
    bytes[0] = 0xB0;
    bytes[1] = (sequence << 4) | (mode_a << 2) | mode_b;
    bytes[2] = value_a;
    bytes[3] = value_b;
    for (index, wave) in waves.iter().enumerate() {
        let encoded = wave
            .as_ref()
            .map(V3WaveFrame::from_wave_frame)
            .map(V3WaveFrame::bytes)
            .unwrap_or(NO_OUTPUT);
        bytes[4 + index * 8..12 + index * 8].copy_from_slice(&encoded);
    }
    bytes
}

pub fn query_strength(sequence: u8) -> [u8; 20] {
    b0(
        sequence,
        [StrengthChange::Increase(0), StrengthChange::Unchanged],
        [None, None],
    )
}

pub fn relative(channel: Channel, delta: i32) -> Option<[StrengthChange; 2]> {
    if delta == 0 || delta.unsigned_abs() > 200 {
        return None;
    }
    let mut changes = [StrengthChange::Unchanged; 2];
    changes[channel.as_v4() as usize] = if delta > 0 {
        StrengthChange::Increase(delta as u8)
    } else {
        StrengthChange::Decrease(delta.unsigned_abs() as u8)
    };
    Some(changes)
}

pub fn bf(limits: [u8; 2], frequencies: [u8; 2], pulse_widths: [u8; 2]) -> [u8; 7] {
    [
        0xBF,
        limits[0],
        limits[1],
        frequencies[0],
        frequencies[1],
        pulse_widths[0],
        pulse_widths[1],
    ]
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Feedback {
    Strength { sequence: u8, values: [u8; 2] },
    StandardMode,
    WheelProtection(u8),
    Unknown,
}

pub fn feedback(bytes: &[u8]) -> Feedback {
    match bytes {
        [0xB1, sequence, a, b] if *sequence <= 15 && *a <= 200 && *b <= 200 => Feedback::Strength {
            sequence: *sequence,
            values: [*a, *b],
        },
        bytes if bytes == STANDARD_MODE_ACK => Feedback::StandardMode,
        [0xC4, value] if (1..=50).contains(value) || *value == 255 => {
            Feedback::WheelProtection(*value)
        }
        _ => Feedback::Unknown,
    }
}

pub fn next_sequence(previous: u8) -> u8 {
    if previous >= 15 { 1 } else { previous + 1 }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::WaveSample;

    #[test]
    fn initialization_query_preserves_strength_and_mutes_both_channels() {
        let query = query_strength(1);
        assert_eq!(&query[..4], &[0xB0, 0x14, 0, 0]);
        assert_eq!(&query[4..12], &NO_OUTPUT);
        assert_eq!(&query[12..], &NO_OUTPUT);
    }

    #[test]
    fn b0_contains_both_channels_in_official_order() {
        let wave = WaveFrame::new([
            WaveSample::new(10, 0).unwrap(),
            WaveSample::new(10, 10).unwrap(),
            WaveSample::new(20, 20).unwrap(),
            WaveSample::new(30, 30).unwrap(),
        ]);
        let bytes = b0(2, relative(Channel::A, -17).unwrap(), [Some(wave), None]);
        assert_eq!(
            &bytes[..12],
            &[0xB0, 0x28, 17, 0, 10, 10, 20, 30, 0, 10, 20, 30]
        );
        assert_eq!(&bytes[12..], &NO_OUTPUT);
    }

    #[test]
    fn malformed_feedback_cannot_change_domain_strength() {
        for bytes in [
            vec![0xB1, 1, 255, 0],
            vec![0xB1, 16, 0, 0],
            vec![0xB1, 1, 0],
        ] {
            assert_eq!(feedback(&bytes), Feedback::Unknown);
        }
        assert_eq!(
            feedback(&[0xB1, 0, 70, 20]),
            Feedback::Strength {
                sequence: 0,
                values: [70, 20]
            }
        );
    }

    #[test]
    fn sequence_wraps_without_using_unacknowledged_zero() {
        let mut sequence = 0;
        for expected in (1..=15).chain(1..=15) {
            sequence = next_sequence(sequence);
            assert_eq!(sequence, expected);
        }
    }
}
