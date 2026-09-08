// probe A: a production fn placed AFTER the test module
#[cfg(test)]
mod tests {
    #[test]
    fn t() {}
}

pub fn after_tests() -> u8 {
    let v: Option<u8> = None;
    v.unwrap()
}
