mod async_trait;
mod testing;

use proc_macro::TokenStream;

#[proc_macro_attribute]
pub fn async_trait(args: TokenStream, input: TokenStream) -> TokenStream {
    async_trait::async_trait(args, input)
}

#[proc_macro_attribute]
pub fn async_test(args: TokenStream, input: TokenStream) -> TokenStream {
    testing::async_test(args, input)
}

#[proc_macro_attribute]
pub fn async_test_only_wasm(args: TokenStream, input: TokenStream) -> TokenStream {
    testing::async_test_only_wasm(args, input)
}

#[proc_macro_attribute]
pub fn async_test_all(args: TokenStream, input: TokenStream) -> TokenStream {
    testing::async_test_all(args, input)
}

#[proc_macro_attribute]
pub fn test(args: TokenStream, input: TokenStream) -> TokenStream {
    testing::test(args, input)
}

#[proc_macro_attribute]
pub fn test_all(args: TokenStream, input: TokenStream) -> TokenStream {
    testing::test_all(args, input)
}
