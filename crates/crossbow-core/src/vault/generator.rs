//! Random passwords from the OS CSPRNG, with unbiased choices and at least
//! one character from each chosen class.

use zeroize::Zeroizing;

pub const MIN_LENGTH: usize = 12;
pub const MAX_LENGTH: usize = 64;
const LOWERCASE: &str = "abcdefghijklmnopqrstuvwxyz";
const UPPERCASE: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZ";
const DIGITS: &str = "0123456789";
/// No quotes, backslash or space, which forms and shells often mangle.
const SYMBOLS: &str = "!#$%&*+-=?@^_~|";
const AMBIGUOUS: &str = "0O1lI|";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PasswordRules {
    pub length: usize,
    pub lowercase: bool,
    pub uppercase: bool,
    pub digits: bool,
    pub symbols: bool,
    /// Leave out characters that are easy to misread, such as 0/O and 1/l/I.
    pub avoid_ambiguous: bool,
}

impl Default for PasswordRules {
    fn default() -> Self {
        PasswordRules { length: 20, lowercase: true, uppercase: true, digits: true, symbols: true, avoid_ambiguous: true }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum GeneratorError {
    #[error("Choose a length from 12 to 64 characters.")]
    Length,
    #[error("Choose at least one kind of character.")]
    NoClasses,
    #[error("The system random number generator is unavailable.")]
    Random,
}

fn classes(rules: &PasswordRules) -> Vec<Vec<u8>> {
    [(rules.lowercase, LOWERCASE), (rules.uppercase, UPPERCASE), (rules.digits, DIGITS), (rules.symbols, SYMBOLS)]
        .into_iter()
        .filter(|(on, _)| *on)
        .map(|(_, set)| set.bytes().filter(|b| !(rules.avoid_ambiguous && AMBIGUOUS.as_bytes().contains(b))).collect())
        .collect()
}

/// Every character `rules` may produce.
pub fn alphabet(rules: &PasswordRules) -> String {
    String::from_utf8(classes(rules).concat()).expect("ASCII")
}

/// Uniform indices below `n` from the OS CSPRNG, by rejection sampling.
struct Dice {
    buffer: Zeroizing<[u8; 256]>,
    used: usize,
}

impl Dice {
    fn new() -> Dice {
        Dice { buffer: Zeroizing::new([0; 256]), used: 256 }
    }

    fn next_u32(&mut self) -> Result<u32, GeneratorError> {
        if self.used + 4 > self.buffer.len() {
            getrandom::fill(self.buffer.as_mut_slice()).map_err(|_| GeneratorError::Random)?;
            self.used = 0;
        }
        let bytes = [self.buffer[self.used], self.buffer[self.used + 1], self.buffer[self.used + 2], self.buffer[self.used + 3]];
        self.used += 4;
        Ok(u32::from_le_bytes(bytes))
    }

    fn below(&mut self, n: usize) -> Result<usize, GeneratorError> {
        let n = n as u32;
        // Values at or above the largest multiple of n would favour small results.
        let zone = u32::MAX - u32::MAX % n;
        loop {
            let value = self.next_u32()?;
            if value < zone { return Ok((value % n) as usize); }
        }
    }
}

/// A password of exactly `rules.length` characters with at least one from
/// each chosen class, shuffled so those characters have no fixed position.
pub fn generate(rules: &PasswordRules) -> Result<Zeroizing<String>, GeneratorError> {
    if !(MIN_LENGTH..=MAX_LENGTH).contains(&rules.length) {
        return Err(GeneratorError::Length);
    }
    let classes = classes(rules);
    if classes.is_empty() {
        return Err(GeneratorError::NoClasses);
    }
    let all = classes.concat();
    let mut dice = Dice::new();
    let mut bytes = Zeroizing::new(Vec::with_capacity(rules.length));
    for class in &classes {
        bytes.push(class[dice.below(class.len())?]);
    }
    while bytes.len() < rules.length {
        bytes.push(all[dice.below(all.len())?]);
    }
    // Fisher–Yates.
    for i in (1..bytes.len()).rev() {
        let j = dice.below(i + 1)?;
        bytes.swap(i, j);
    }
    let mut password = Zeroizing::new(String::with_capacity(rules.length));
    password.extend(bytes.iter().map(|&b| b as char));
    Ok(password)
}
