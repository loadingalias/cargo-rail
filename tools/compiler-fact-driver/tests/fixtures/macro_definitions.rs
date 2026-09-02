macro_rules! define_public_unit {
  (
    $(#[$meta:meta])*
    $vis:vis struct $name:ident;
  ) => {
    $(#[$meta])*
    $vis struct $name;
  };
}
