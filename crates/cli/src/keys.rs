// SPDX-License-Identifier: EUPL-1.2
/// Linux input-event codes to USB HID Keyboard/Keypad usage IDs.
/// Values are protocol constants, never keyboard names or layouts.
pub fn usage(code: u16) -> Option<u16> {
    const STANDARD: [u16; 83] = [
        41, 30, 31, 32, 33, 34, 35, 36, 37, 38, 39, 45, 46, 42, 43, 20, 26, 8, 21, 23, 28, 24, 12,
        18, 19, 47, 48, 40, 224, 4, 22, 7, 9, 10, 11, 13, 14, 15, 51, 52, 53, 225, 49, 29, 27, 6,
        25, 5, 17, 16, 54, 55, 56, 229, 85, 226, 44, 57, 58, 59, 60, 61, 62, 63, 64, 65, 66, 67,
        83, 71, 95, 96, 97, 86, 92, 93, 94, 87, 89, 90, 91, 98, 99,
    ];
    if (1..=83).contains(&code) {
        return Some(STANDARD[code as usize - 1]);
    }
    Some(match code {
        86 => 100,
        87 => 68,
        88 => 69,
        89 => 135,
        96 => 88,
        97 => 228,
        98 => 84,
        99 => 70,
        100 => 230,
        102 => 74,
        103 => 82,
        104 => 75,
        105 => 80,
        106 => 79,
        107 => 77,
        108 => 81,
        109 => 78,
        110 => 73,
        111 => 76,
        119 => 72,
        122 => 144,
        123 => 145,
        124 => 137,
        125 => 227,
        126 => 231,
        127 => 101,
        _ => return None,
    })
}
