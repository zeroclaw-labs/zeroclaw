use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::{sol, SolCall};

sol! {
    function transfer(address to, uint256 amount) external returns (bool);
    function balanceOf(address account) external view returns (uint256);
    function decimals() external view returns (uint8);
    function symbol() external view returns (string);
}

pub fn encode_transfer(to: Address, amount: U256) -> Bytes {
    transferCall { to, amount }.abi_encode().into()
}

pub fn encode_balance_of(account: Address) -> Bytes {
    balanceOfCall { account }.abi_encode().into()
}

pub fn encode_decimals() -> Bytes {
    decimalsCall {}.abi_encode().into()
}

pub fn encode_symbol() -> Bytes {
    symbolCall {}.abi_encode().into()
}

pub fn decode_balance_of(data: &[u8]) -> anyhow::Result<U256> {
    Ok(balanceOfCall::abi_decode_returns(data)?)
}

pub fn decode_decimals(data: &[u8]) -> anyhow::Result<u8> {
    Ok(decimalsCall::abi_decode_returns(data)?)
}

pub fn decode_symbol(data: &[u8]) -> anyhow::Result<String> {
    Ok(symbolCall::abi_decode_returns(data)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_transfer_selector() {
        let data = encode_transfer(Address::ZERO, U256::from(100u64));
        assert_eq!(&data[..4], &[0xa9, 0x05, 0x9c, 0xbb]);
    }

    #[test]
    fn encode_balance_of_roundtrip() {
        let addr: Address = "0x0000000000000000000000000000000000000001"
            .parse()
            .unwrap();
        let encoded = encode_balance_of(addr);
        assert_eq!(&encoded[..4], &[0x70, 0xa0, 0x82, 0x31]);
    }

    #[test]
    fn decode_balance_of_padded() {
        let mut data = vec![0u8; 32];
        data[31] = 42;
        let balance = decode_balance_of(&data).unwrap();
        assert_eq!(balance, U256::from(42u64));
    }

    #[test]
    fn decode_decimals_value() {
        let mut data = vec![0u8; 32];
        data[31] = 18;
        let decimals = decode_decimals(&data).unwrap();
        assert_eq!(decimals, 18);
    }
}
