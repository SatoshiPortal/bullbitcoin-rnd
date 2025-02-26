use crate::error::Error;
use lightning_invoice::Bolt11Invoice;
use lnurl::lightning_address::LightningAddress;
use lnurl::pay::LnURLPayInvoice;
use lnurl::withdraw::WithdrawalResponse;
use lnurl::{lnurl::LnUrl, Builder, LnUrlResponse};
use std::cmp::max;
use std::str::FromStr;

pub fn validate_lnurl(string: &str) -> bool {
    let string = string.to_lowercase();
    match LnUrl::from_str(&string) {
        Ok(lnurl) => true,
        Err(_) => match LightningAddress::from_str(&string) {
            Ok(lightning_address) => true,
            Err(_) => false,
        },
    }
}

pub async fn fetch_invoice(address: &str, amount_msats: u64) -> Result<String, Error> {
    let address = address.to_lowercase();
    let lnurl = match LnUrl::from_str(&address) {
        Ok(lnurl) => lnurl,
        Err(_) => match LightningAddress::from_str(&address) {
            Ok(lightning_address) => lightning_address.lnurl(),
            Err(_) => return Err(Error::Generic("Not a valid LnUrl or LnAddress".to_string())),
        },
    };

    let client = Builder::default()
        .build_async()
        .map_err(|e| Error::Generic(e.to_string()))?;
    let res = client
        .make_request(&lnurl.url)
        .await
        .map_err(|e| Error::HTTP(e.to_string()))?;

    match res {
        LnUrlResponse::LnUrlPayResponse(pay) => {
            let pay_result = client
                .get_invoice(&pay, amount_msats, None, None)
                .await
                .map_err(|e| Error::HTTP(e.to_string()))?;
            let invoice = Bolt11Invoice::from_str(pay_result.invoice()).map_err(Error::Bolt11)?;

            if invoice.amount_milli_satoshis() != Some(amount_msats) {
                return Err(Error::Generic(
                    "Invoice amount doesn't match requested amount".to_string(),
                ));
            }

            Ok(pay_result.invoice().to_string())
        }
        _ => Err(Error::Generic("Unexpected response type".to_string())),
    }
}

pub async fn create_withdraw_response(voucher: &str) -> Result<WithdrawalResponse, Error> {
    let lnurl = LnUrl::from_str(&voucher.to_lowercase())
        .map_err(|_| Error::Generic("Invalid LNURL".to_string()))?;

    let client = Builder::default()
        .build_async()
        .map_err(|e| Error::Generic(e.to_string()))?;

    let res = client
        .make_request(&lnurl.url)
        .await
        .map_err(|e| Error::HTTP(e.to_string()))?;

    match res {
        LnUrlResponse::LnUrlWithdrawResponse(withdraw) => Ok(withdraw),
        _ => Err(Error::Generic("Unexpected response type".to_string())),
    }
}

pub async fn process_withdrawal(withdraw: &WithdrawalResponse, invoice: &str) -> Result<(), Error> {
    let client = Builder::default()
        .build_async()
        .map_err(|e| Error::Generic(e.to_string()))?;

    let withdraw_result = client
        .do_withdrawal(withdraw, invoice)
        .await
        .map_err(|e| Error::HTTP(e.to_string()))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn test_address(address: &str, amount_msats: u64, format: &str) {
        let result = fetch_invoice(address, amount_msats).await;

        match result {
            Ok(invoice) => {
                assert!(!invoice.is_empty(), "Invoice should not be empty");
                assert!(
                    invoice.starts_with("lnbc"),
                    "Invoice should start with 'lnbc'"
                );
                println!("Successfully fetched invoice, format : {}", format)
            }
            Err(e) => {
                println!("Error occured with {} format: {}", format, e.message());
            }
        }
    }

    #[macros::async_test_all]
    async fn test_fetch_invoice() {
        let amount_msats = 100000;
        let lnurl = "lnurl1dp68gurn8ghj7um9wfmxjcm99e3k7mf0v9cxj0m385ekvcenxc6r2c35xvukxefcv5mkvv34x5ekzd3ev56nyd3hxqurzepexejxxepnxscrvwfnv9nxzcn9xq6xyefhvgcxxcmyxymnserxfq5fns";
        let uppercase_lnurl = lnurl.to_uppercase();
        assert!(validate_lnurl(lnurl));
        test_address(lnurl, amount_msats, "LNURL");
        test_address(&uppercase_lnurl, amount_msats, "LNURL");

        let email_lnurl = "drunksteel17@walletofsatoshi.com";
        assert!(validate_lnurl(email_lnurl));
        test_address(email_lnurl, amount_msats, "Lightning Address");
    }

    #[ignore = "Requires using an new lnurl-w voucher and invoice to match the max_withdrawble amount"]
    #[macros::async_test_all]
    async fn test_process_withdrawal() {
        let voucher = "LNURL1DP68GURN8GHJ7ER9D4HJUMRWVF5HGUEWVDHK6TMHD96XSERJV9MJ7CTSDYHHVVF0D3H82UNV9AVYS6ZV899XS4J6WFYRV6Z9TQU4GUT9VF48SWY20AR";
        let invoice = "lnbc4u1pnsywcypp5eamm4c3v42vlyr0asmt55muv02zusjp2dy7j6e3kuz5vv3cuyj6scqpjsp56hujjsj4r76gp9gk6y435rz99682uxjx924a06wwqm0av6ezxepq9q7sqqqqqqqqqqqqqqqqqqqsqqqqqysgqdqqmqz9gxqyjw5qrzjqwryaup9lh50kkranzgcdnn2fgvx390wgj5jd07rwr3vxeje0glcllm8u4a8gvusysqqqqlgqqqqqeqqjqtgxt57vzea9xaygxu806xf7w5872n737ptuc6al0plf3544a2f5y2e42j9qv7gvkqkn9k2yxzmew6rr40z2gyq9nu8atj2yt4dlfm3gpjevcgu";
        assert!(validate_lnurl(voucher));
        let withdraw_response = match create_withdraw_response(voucher).await {
            Ok(response) => response,
            Err(e) => {
                println!("Failed to create withdraw response: {:?}", e);
                return;
            }
        };

        let invoice_amount = match Bolt11Invoice::from_str(invoice) {
            Ok(invoice) => invoice.amount_milli_satoshis(),
            Err(e) => {
                println!("Failed to parse invoice: {:?}", e);
                return;
            }
        };

        assert_eq!(
            invoice_amount,
            Option::from(withdraw_response.max_withdrawable),
            "Invoice of {:?} doesn't match with withdrawable {} sats",
            invoice_amount,
            withdraw_response.max_withdrawable
        );
        println!(
            "Successfully created withdraw response{:?}",
            withdraw_response
        );
        let result = process_withdrawal(&withdraw_response, invoice).await;

        assert!(result.is_ok(), "Withdrawal failed: {:?}", result.err());

        println!("Withdrawal test passed successfully");
    }
}
