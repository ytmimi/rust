// See src/libstd/primitive_docs.rs for documentation.

use crate::cmp::Ordering::*;
use crate::cmp::*;

// macro for implementing n-ary tuple functions and operations
macro_rules! tuple_impls {
    ( $( $Tuple:ident( $( $T:ident )+ ) )+ ) => {
        $(
            #[stable(feature = "rust1", since = "1.0.0")]
            impl<$($T:PartialEq),+> PartialEq for ($($T,)+) where last_type!($($T,)+): ?Sized {
                #[inline]
                fn eq(&self, other: &($($T,)+)) -> bool {
                    $( ${ignore(T)} self.${index()} == other.${index()} )&&+
                }
                #[inline]
                fn ne(&self, other: &($($T,)+)) -> bool {
                    $( ${ignore(T)} self.${index()} != other.${index()} )||+
                }
            }

            #[stable(feature = "rust1", since = "1.0.0")]
            impl<$($T:Eq),+> Eq for ($($T,)+) where last_type!($($T,)+): ?Sized {}

            #[stable(feature = "rust1", since = "1.0.0")]
            impl<$($T:PartialOrd + PartialEq),+> PartialOrd for ($($T,)+)
            where
                last_type!($($T,)+): ?Sized
            {
                #[inline]
                fn partial_cmp(&self, other: &($($T,)+)) -> Option<Ordering> {
                    lexical_partial_cmp!($( ${ignore(T)} self.${index()}, other.${index()} ),+)
                }
                #[inline]
                fn lt(&self, other: &($($T,)+)) -> bool {
                    lexical_ord!(lt, $( ${ignore(T)} self.${index()}, other.${index()} ),+)
                }
                #[inline]
                fn le(&self, other: &($($T,)+)) -> bool {
                    lexical_ord!(le, $( ${ignore(T)} self.${index()}, other.${index()} ),+)
                }
                #[inline]
                fn ge(&self, other: &($($T,)+)) -> bool {
                    lexical_ord!(ge, $( ${ignore(T)} self.${index()}, other.${index()} ),+)
                }
                #[inline]
                fn gt(&self, other: &($($T,)+)) -> bool {
                    lexical_ord!(gt, $( ${ignore(T)} self.${index()}, other.${index()} ),+)
                }
            }

            #[stable(feature = "rust1", since = "1.0.0")]
            impl<$($T:Ord),+> Ord for ($($T,)+) where last_type!($($T,)+): ?Sized {
                #[inline]
                fn cmp(&self, other: &($($T,)+)) -> Ordering {
                    lexical_cmp!($( ${ignore(T)} self.${index()}, other.${index()} ),+)
                }
            }

            #[stable(feature = "rust1", since = "1.0.0")]
            impl<$($T:Default),+> Default for ($($T,)+) {
                #[inline]
                fn default() -> ($($T,)+) {
                    ($({ let x: $T = Default::default(); x},)+)
                }
            }
        )+
    }
}

// Constructs an expression that performs a lexical ordering using method $rel.
// The values are interleaved, so the macro invocation for
// `(a1, a2, a3) < (b1, b2, b3)` would be `lexical_ord!(lt, a1, b1, a2, b2,
// a3, b3)` (and similarly for `lexical_cmp`)
macro_rules! lexical_ord {
    ($rel: ident, $a:expr, $b:expr, $($rest_a:expr, $rest_b:expr),+) => {
        if $a != $b { lexical_ord!($rel, $a, $b) }
        else { lexical_ord!($rel, $($rest_a, $rest_b),+) }
    };
    ($rel: ident, $a:expr, $b:expr) => { ($a) . $rel (& $b) };
}

macro_rules! lexical_partial_cmp {
    ($a:expr, $b:expr, $($rest_a:expr, $rest_b:expr),+) => {
        match ($a).partial_cmp(&$b) {
            Some(Equal) => lexical_partial_cmp!($($rest_a, $rest_b),+),
            ordering   => ordering
        }
    };
    ($a:expr, $b:expr) => { ($a).partial_cmp(&$b) };
}

macro_rules! lexical_cmp {
    ($a:expr, $b:expr, $($rest_a:expr, $rest_b:expr),+) => {
        match ($a).cmp(&$b) {
            Equal => lexical_cmp!($($rest_a, $rest_b),+),
            ordering   => ordering
        }
    };
    ($a:expr, $b:expr) => { ($a).cmp(&$b) };
}

macro_rules! last_type {
    ($a:ident,) => { $a };
    ($a:ident, $($rest_a:ident,)+) => { last_type!($($rest_a,)+) };
}

tuple_impls! {
    Tuple1(A)
    Tuple2(A B)
    Tuple3(A B C)
    Tuple4(A B C D)
    Tuple5(A B C D E)
    Tuple6(A B C D E F)
    Tuple7(A B C D E F G)
    Tuple8(A B C D E F G H)
    Tuple9(A B C D E F G H I)
    Tuple10(A B C D E F G H I J)
    Tuple11(A B C D E F G H I J K)
    Tuple12(A B C D E F G H I J K L)
}
