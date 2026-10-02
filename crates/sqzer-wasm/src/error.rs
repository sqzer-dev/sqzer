//! `SqzerError`, what every export throws: a JavaScript `Error` carrying
//! the `sqzer_core::Error` variant as `kind` (ADR-0011 D3).

use js_sys::{Array, Reflect};
use sqzer::core::Error;
use wasm_bindgen::prelude::*;

#[wasm_bindgen(typescript_custom_section)]
const SQZER_ERROR: &str = r#"
/**
 * What every function of this package throws. `message` is the text the
 * `sqzer` command line prints for the same failure.
 */
export interface SqzerError extends Error {
    name: "SqzerError";
    /** Which failure it is: the `sqzer_core::Error` variant. */
    kind:
        | "UnknownFormat"
        | "DecoderUnavailable"
        | "EncoderUnavailable"
        | "TooLarge"
        | "InvalidInput"
        | "InvalidParams"
        | "Unsupported"
        | "Codec"
        | "Transform"
        | "Other";
    /**
     * For `DecoderUnavailable` and `EncoderUnavailable`: the Cargo features
     * of a `sqzer` build that has the backend. Empty when no build has.
     */
    availableIn?: string[];
}
"#;

/// The `Err` of every export. Built from a [`sqzer::core::Error`] by `?`.
#[derive(Debug)]
pub struct SqzerError(JsValue);

impl SqzerError {
    /// An exception a JavaScript callback threw, to throw again as it is.
    #[must_use]
    pub fn thrown(exception: JsValue) -> Self {
        Self(exception)
    }

    /// The error for `e`, with `note` appended to its message.
    #[must_use]
    pub fn with_note(e: &Error, note: &str) -> Self {
        Self::new(e, &format!("{e}; {note}"))
    }

    fn new(e: &Error, message: &str) -> Self {
        let error = js_sys::Error::new(message);
        error.set_name("SqzerError");
        set(&error, "kind", &kind(e).into());
        if let Error::DecoderUnavailable { available_in, .. }
        | Error::EncoderUnavailable { available_in, .. } = e
        {
            let features: Array = available_in.iter().copied().map(JsValue::from).collect();
            set(&error, "availableIn", &features);
        }
        Self(error.into())
    }
}

/// `sqzer_core::Error` is `non_exhaustive`; a variant added there reads
/// `Other` here until it is named.
fn kind(e: &Error) -> &'static str {
    match e {
        Error::UnknownFormat => "UnknownFormat",
        Error::DecoderUnavailable { .. } => "DecoderUnavailable",
        Error::EncoderUnavailable { .. } => "EncoderUnavailable",
        Error::TooLarge { .. } => "TooLarge",
        Error::InvalidInput(_) => "InvalidInput",
        Error::InvalidParams(_) => "InvalidParams",
        Error::Unsupported { .. } => "Unsupported",
        Error::Codec(_) => "Codec",
        Error::Transform { .. } => "Transform",
        _ => "Other",
    }
}

fn set(error: &js_sys::Error, key: &str, value: &JsValue) {
    // Setting a property on a fresh `Error` cannot fail.
    let _ = Reflect::set(error, &key.into(), value);
}

impl From<Error> for SqzerError {
    fn from(e: Error) -> Self {
        Self::new(&e, &e.to_string())
    }
}

/// A record that would not serialise. None of them can fail to.
impl From<tsify::Error> for SqzerError {
    fn from(e: tsify::Error) -> Self {
        Self(JsError::new(&e.to_string()).into())
    }
}

impl From<SqzerError> for JsValue {
    fn from(e: SqzerError) -> Self {
        e.0
    }
}

/// What a JavaScript exception says, for a message that quotes it.
pub fn describe(thrown: &JsValue) -> String {
    thrown
        .dyn_ref::<js_sys::Error>()
        .map(|e| String::from(e.message()))
        .or_else(|| thrown.as_string())
        .unwrap_or_else(|| format!("{thrown:?}"))
}
