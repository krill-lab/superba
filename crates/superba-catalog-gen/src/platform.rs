//! `cfg!` evaluation for the three desktop targets Superba supports.
//!
//! Only the predicates the feature registry uses are understood; anything else is
//! an error so the generator is extended instead of guessing.

use anyhow::{Result, bail};
use superba_catalog::Os;
use syn::{Expr, Lit, Meta, punctuated::Punctuated, token::Comma};

/// A value that may differ per platform.
#[derive(Clone, Debug, PartialEq)]
pub enum PerOs<T> {
    Uniform(T),
    ByOs { macos: T, linux: T, windows: T },
}

impl<T: Clone + PartialEq> PerOs<T> {
    pub fn from_fn(mut f: impl FnMut(Os) -> Result<T>) -> Result<Self> {
        let macos = f(Os::Macos)?;
        let linux = f(Os::Linux)?;
        let windows = f(Os::Windows)?;
        if macos == linux && linux == windows {
            Ok(PerOs::Uniform(macos))
        } else {
            Ok(PerOs::ByOs {
                macos,
                linux,
                windows,
            })
        }
    }

    pub fn get(&self, os: Os) -> &T {
        match self {
            PerOs::Uniform(v) => v,
            PerOs::ByOs {
                macos,
                linux,
                windows,
            } => match os {
                Os::Macos => macos,
                Os::Linux => linux,
                Os::Windows => windows,
            },
        }
    }
}

/// Evaluate the predicate inside `cfg!(...)` for a release build on `os`.
pub fn eval_cfg_meta(meta: &Meta, os: Os) -> Result<bool> {
    match meta {
        Meta::Path(p) => {
            let id = p.get_ident().map(|i| i.to_string()).unwrap_or_default();
            Ok(match id.as_str() {
                "windows" => os == Os::Windows,
                "unix" => os != Os::Windows,
                // Release builds (what users run) have no debug assertions / tests.
                "debug_assertions" | "test" => false,
                other => bail!("unsupported cfg flag `{other}`"),
            })
        }
        Meta::NameValue(nv) => {
            let name = nv
                .path
                .get_ident()
                .map(|i| i.to_string())
                .unwrap_or_default();
            let Expr::Lit(lit) = &nv.value else {
                bail!("unsupported cfg value for `{name}`");
            };
            let Lit::Str(s) = &lit.lit else {
                bail!("unsupported cfg literal for `{name}`");
            };
            let v = s.value();
            Ok(match name.as_str() {
                "target_os" => v == os.as_str(),
                "target_family" => match v.as_str() {
                    "unix" => os != Os::Windows,
                    "windows" => os == Os::Windows,
                    _ => false,
                },
                "target_vendor" => v == "apple" && os == Os::Macos,
                // `feature = ".."`: features of the codex crates are not known here;
                // fail loudly so the generator is extended instead of guessing.
                other => bail!("unsupported cfg key `{other}`"),
            })
        }
        Meta::List(list) => {
            let name = list
                .path
                .get_ident()
                .map(|i| i.to_string())
                .unwrap_or_default();
            let args: Punctuated<Meta, Comma> =
                list.parse_args_with(Punctuated::parse_terminated)?;
            match name.as_str() {
                "any" => {
                    for a in &args {
                        if eval_cfg_meta(a, os)? {
                            return Ok(true);
                        }
                    }
                    Ok(false)
                }
                "all" => {
                    for a in &args {
                        if !eval_cfg_meta(a, os)? {
                            return Ok(false);
                        }
                    }
                    Ok(true)
                }
                "not" => {
                    if args.len() != 1 {
                        bail!("cfg not() takes one predicate");
                    }
                    Ok(!eval_cfg_meta(&args[0], os)?)
                }
                other => bail!("unsupported cfg combinator `{other}`"),
            }
        }
    }
}

/// Evaluate a boolean const expression: `true`, `cfg!(..)`, `!e`, `a && b`, `a || b`,
/// `(e)`, `if c { a } else { b }`.
pub fn eval_bool_expr(expr: &Expr, os: Os) -> Result<bool> {
    match expr {
        Expr::Lit(l) => match &l.lit {
            Lit::Bool(b) => Ok(b.value),
            _ => bail!("non-bool literal"),
        },
        Expr::Macro(m) if m.mac.path.is_ident("cfg") => {
            let meta: Meta = m.mac.parse_body()?;
            eval_cfg_meta(&meta, os)
        }
        Expr::Unary(u) if matches!(u.op, syn::UnOp::Not(_)) => Ok(!eval_bool_expr(&u.expr, os)?),
        Expr::Binary(b) => match b.op {
            syn::BinOp::And(_) => Ok(eval_bool_expr(&b.left, os)? && eval_bool_expr(&b.right, os)?),
            syn::BinOp::Or(_) => Ok(eval_bool_expr(&b.left, os)? || eval_bool_expr(&b.right, os)?),
            _ => bail!("unsupported binary operator"),
        },
        Expr::Paren(p) => eval_bool_expr(&p.expr, os),
        Expr::Group(g) => eval_bool_expr(&g.expr, os),
        Expr::If(i) => {
            let cond = eval_bool_expr(&i.cond, os)?;
            let branch = if cond {
                block_tail(&i.then_branch)?
            } else {
                match &i.else_branch {
                    Some((_, e)) => else_tail(e)?,
                    None => bail!("if without else"),
                }
            };
            eval_bool_expr(branch, os)
        }
        other => bail!("unsupported bool expression `{}`", quote::quote!(#other)),
    }
}

/// The trailing expression of a `{ expr }` block.
pub fn block_tail(b: &syn::Block) -> Result<&Expr> {
    match b.stmts.as_slice() {
        [syn::Stmt::Expr(e, None)] => Ok(e),
        _ => bail!("expected a block with a single tail expression"),
    }
}

/// The expression of an `else` branch (`else { e }` or `else if ..`).
pub fn else_tail(e: &Expr) -> Result<&Expr> {
    match e {
        Expr::Block(b) => block_tail(&b.block),
        Expr::If(_) => Ok(e),
        _ => bail!("unsupported else branch"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eval(src: &str, os: Os) -> Result<bool> {
        eval_bool_expr(&syn::parse_str::<Expr>(src)?, os)
    }

    #[test]
    fn cfg_predicates_per_os() {
        assert!(eval("cfg!(windows)", Os::Windows).unwrap());
        assert!(!eval("cfg!(windows)", Os::Linux).unwrap());
        assert!(eval("cfg!(unix)", Os::Macos).unwrap());
        assert!(eval(r#"cfg!(target_os = "macos")"#, Os::Macos).unwrap());
        assert!(!eval(r#"cfg!(target_os = "macos")"#, Os::Linux).unwrap());
        assert!(
            eval(
                r#"cfg!(any(target_os = "macos", target_os = "linux", target_os = "windows"))"#,
                Os::Windows
            )
            .unwrap()
        );
        assert!(eval(r#"cfg!(all(unix, not(target_os = "macos")))"#, Os::Linux).unwrap());
        assert!(!eval(r#"cfg!(all(unix, not(target_os = "macos")))"#, Os::Macos).unwrap());
        assert!(eval("!cfg!(windows) && true", Os::Linux).unwrap());
        assert!(eval("if cfg!(windows) { false } else { true }", Os::Linux).unwrap());
        assert!(!eval("cfg!(debug_assertions)", Os::Linux).unwrap());
    }

    #[test]
    fn unknown_predicates_fail_loudly() {
        assert!(eval(r#"cfg!(feature = "x")"#, Os::Linux).is_err());
        assert!(eval("cfg!(target_pointer_width_is_weird)", Os::Linux).is_err());
        assert!(eval("some_fn()", Os::Linux).is_err());
        assert!(eval("1 + 1", Os::Linux).is_err());
    }
}
