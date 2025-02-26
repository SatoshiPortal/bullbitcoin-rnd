use crate::error::Error;

pub enum Fee {
    // In sat/vByte
    Relative(f64),
    // In satoshis
    Absolute(u64),
}

pub(crate) fn create_tx_with_fee<T, F, S>(
    fee: Fee,
    tx_constructor: F,
    get_vsize: S,
) -> Result<T, Error>
where
    F: Fn(u64) -> Result<T, Error>,
    S: Fn(T) -> usize,
{
    match fee {
        Fee::Relative(fee) => {
            let vsize = get_vsize(tx_constructor(1)?);
            // Round up to make sure we are not under the min relay fee
            tx_constructor((vsize as f64 * fee).ceil() as u64)
        }
        Fee::Absolute(fee) => tx_constructor(fee),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct StubTx {
        fee: u64,
    }

    #[macros::test_all]
    fn test_create_tx_with_fee_relative() {
        let fee = 0.1;
        let vsize = 42;
        let tx =
            create_tx_with_fee(Fee::Relative(fee), |fee| Ok(StubTx { fee }), |_| vsize).unwrap();
        assert_eq!(tx.fee, 5);
    }

    #[macros::test_all]
    fn test_create_tx_with_fee_absolute() {
        let fee = 21;
        let tx = create_tx_with_fee(Fee::Absolute(fee), |fee| Ok(StubTx { fee }), |_| 42).unwrap();
        assert_eq!(tx.fee, fee);
    }
}
