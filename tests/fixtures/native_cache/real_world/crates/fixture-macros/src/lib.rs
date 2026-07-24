use proc_macro::TokenStream;

#[proc_macro_attribute]
pub fn fixture_component(_attribute: TokenStream, item: TokenStream) -> TokenStream {
  item
}
