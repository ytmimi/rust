use crate::abi::{self, Abi, Align, FieldsShape, Size};
use crate::abi::{HasDataLayout, TyAbiInterface, TyAndLayout};
use crate::spec::{self, HasTargetSpec};
use rustc_span::Symbol;
use std::fmt;
use std::str::FromStr;

mod aarch64;
mod amdgpu;
mod arm;
mod avr;
mod bpf;
mod csky;
mod hexagon;
mod loongarch;
mod m68k;
mod mips;
mod mips64;
mod msp430;
mod nvptx64;
mod powerpc;
mod powerpc64;
mod riscv;
mod s390x;
mod sparc;
mod sparc64;
mod wasm;
mod x86;
mod x86_64;
mod x86_win64;

#[derive(Clone, PartialEq, Eq, Hash, Debug, HashStable_Generic)]
pub enum PassMode {
    /// Ignore the argument.
    ///
    /// The argument is either uninhabited or a ZST.
    Ignore,
    /// Pass the argument directly.
    ///
    /// The argument has a layout abi of `Scalar` or `Vector`.
    /// Unfortunately due to past mistakes, in rare cases on wasm, it can also be `Aggregate`.
    /// This is bad since it leaks LLVM implementation details into the ABI.
    /// (Also see <https://github.com/rust-lang/rust/issues/115666>.)
    Direct(ArgAttributes),
    /// Pass a pair's elements directly in two arguments.
    ///
    /// The argument has a layout abi of `ScalarPair`.
    Pair(ArgAttributes, ArgAttributes),
    /// Pass the argument after casting it. See the `CastTarget` docs for details.
    ///
    /// `pad_i32` indicates if a `Reg::i32()` dummy argument is emitted before the real argument.
    Cast { pad_i32: bool, cast: Box<CastTarget> },
    /// Pass the argument indirectly via a hidden pointer.
    ///
    /// The `meta_attrs` value, if any, is for the metadata (vtable or length) of an unsized
    /// argument. (This is the only mode that supports unsized arguments.)
    ///
    /// `on_stack` defines that the value should be passed at a fixed stack offset in accordance to
    /// the ABI rather than passed using a pointer. This corresponds to the `byval` LLVM argument
    /// attribute. The `byval` argument will use a byte array with the same size as the Rust type
    /// (which ensures that padding is preserved and that we do not rely on LLVM's struct layout),
    /// and will use the alignment specified in `attrs.pointee_align` (if `Some`) or the type's
    /// alignment (if `None`). This means that the alignment will not always
    /// match the Rust type's alignment; see documentation of `make_indirect_byval` for more info.
    ///
    /// `on_stack` cannot be true for unsized arguments, i.e., when `meta_attrs` is `Some`.
    Indirect { attrs: ArgAttributes, meta_attrs: Option<ArgAttributes>, on_stack: bool },
}

impl PassMode {
    /// Checks if these two `PassMode` are equal enough to be considered "the same for all
    /// function call ABIs". However, the `Layout` can also impact ABI decisions,
    /// so that needs to be compared as well!
    pub fn eq_abi(&self, other: &Self) -> bool {
        match (self, other) {
            (PassMode::Ignore, PassMode::Ignore) => true,
            (PassMode::Direct(a1), PassMode::Direct(a2)) => a1.eq_abi(a2),
            (PassMode::Pair(a1, b1), PassMode::Pair(a2, b2)) => a1.eq_abi(a2) && b1.eq_abi(b2),
            (
                PassMode::Cast { cast: c1, pad_i32: pad1 },
                PassMode::Cast { cast: c2, pad_i32: pad2 },
            ) => c1.eq_abi(c2) && pad1 == pad2,
            (
                PassMode::Indirect { attrs: a1, meta_attrs: None, on_stack: s1 },
                PassMode::Indirect { attrs: a2, meta_attrs: None, on_stack: s2 },
            ) => a1.eq_abi(a2) && s1 == s2,
            (
                PassMode::Indirect { attrs: a1, meta_attrs: Some(e1), on_stack: s1 },
                PassMode::Indirect { attrs: a2, meta_attrs: Some(e2), on_stack: s2 },
            ) => a1.eq_abi(a2) && e1.eq_abi(e2) && s1 == s2,
            _ => false,
        }
    }
}

// Hack to disable non_upper_case_globals only for the bitflags! and not for the rest
// of this module
pub use attr_impl::ArgAttribute;

#[allow(non_upper_case_globals)]
#[allow(unused)]
mod attr_impl {
    // The subset of llvm::Attribute needed for arguments, packed into a bitfield.
    #[derive(Clone, Copy, Default, Hash, PartialEq, Eq, HashStable_Generic)]
    pub struct ArgAttribute(u8);
    bitflags::bitflags! {
        impl ArgAttribute: u8 {
            const NoAlias   = 1 << 1;
            const NoCapture = 1 << 2;
            const NonNull   = 1 << 3;
            const ReadOnly  = 1 << 4;
            const InReg     = 1 << 5;
            const NoUndef = 1 << 6;
        }
    }
    rustc_data_structures::external_bitflags_debug! { ArgAttribute }
}

/// Sometimes an ABI requires small integers to be extended to a full or partial register. This enum
/// defines if this extension should be zero-extension or sign-extension when necessary. When it is
/// not necessary to extend the argument, this enum is ignored.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, HashStable_Generic)]
pub enum ArgExtension {
    None,
    Zext,
    Sext,
}

/// A compact representation of LLVM attributes (at least those relevant for this module)
/// that can be manipulated without interacting with LLVM's Attribute machinery.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, HashStable_Generic)]
pub struct ArgAttributes {
    pub regular: ArgAttribute,
    pub arg_ext: ArgExtension,
    /// The minimum size of the pointee, guaranteed to be valid for the duration of the whole call
    /// (corresponding to LLVM's dereferenceable and dereferenceable_or_null attributes).
    pub pointee_size: Size,
    pub pointee_align: Option<Align>,
}

impl ArgAttributes {
    pub fn new() -> Self {
        ArgAttributes {
            regular: ArgAttribute::default(),
            arg_ext: ArgExtension::None,
            pointee_size: Size::ZERO,
            pointee_align: None,
        }
    }

    pub fn ext(&mut self, ext: ArgExtension) -> &mut Self {
        assert!(
            self.arg_ext == ArgExtension::None || self.arg_ext == ext,
            "cannot set {:?} when {:?} is already set",
            ext,
            self.arg_ext
        );
        self.arg_ext = ext;
        self
    }

    pub fn set(&mut self, attr: ArgAttribute) -> &mut Self {
        self.regular |= attr;
        self
    }

    pub fn contains(&self, attr: ArgAttribute) -> bool {
        self.regular.contains(attr)
    }

    /// Checks if these two `ArgAttributes` are equal enough to be considered "the same for all
    /// function call ABIs".
    pub fn eq_abi(&self, other: &Self) -> bool {
        // There's only one regular attribute that matters for the call ABI: InReg.
        // Everything else is things like noalias, dereferenceable, nonnull, ...
        // (This also applies to pointee_size, pointee_align.)
        if self.regular.contains(ArgAttribute::InReg) != other.regular.contains(ArgAttribute::InReg)
        {
            return false;
        }
        // We also compare the sign extension mode -- this could let the callee make assumptions
        // about bits that conceptually were not even passed.
        if self.arg_ext != other.arg_ext {
            return false;
        }
        return true;
    }
}

#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, HashStable_Generic)]
pub enum RegKind {
    Integer,
    Float,
    Vector,
}

#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, HashStable_Generic)]
pub struct Reg {
    pub kind: RegKind,
    pub size: Size,
}

macro_rules! reg_ctor {
    ($name:ident, $kind:ident, $bits:expr) => {
        pub fn $name() -> Reg {
            Reg { kind: RegKind::$kind, size: Size::from_bits($bits) }
        }
    };
}

impl Reg {
    reg_ctor!(i8, Integer, 8);
    reg_ctor!(i16, Integer, 16);
    reg_ctor!(i32, Integer, 32);
    reg_ctor!(i64, Integer, 64);
    reg_ctor!(i128, Integer, 128);

    reg_ctor!(f32, Float, 32);
    reg_ctor!(f64, Float, 64);
}

impl Reg {
    pub fn align<C: HasDataLayout>(&self, cx: &C) -> Align {
        let dl = cx.data_layout();
        match self.kind {
            RegKind::Integer => match self.size.bits() {
                1 => dl.i1_align.abi,
                2..=8 => dl.i8_align.abi,
                9..=16 => dl.i16_align.abi,
                17..=32 => dl.i32_align.abi,
                33..=64 => dl.i64_align.abi,
                65..=128 => dl.i128_align.abi,
                _ => panic!("unsupported integer: {self:?}"),
            },
            RegKind::Float => match self.size.bits() {
                32 => dl.f32_align.abi,
                64 => dl.f64_align.abi,
                _ => panic!("unsupported float: {self:?}"),
            },
            RegKind::Vector => dl.vector_align(self.size).abi,
        }
    }
}

/// An argument passed entirely registers with the
/// same kind (e.g., HFA / HVA on PPC64 and AArch64).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, HashStable_Generic)]
pub struct Uniform {
    pub unit: Reg,

    /// The total size of the argument, which can be:
    /// * equal to `unit.size` (one scalar/vector),
    /// * a multiple of `unit.size` (an array of scalar/vectors),
    /// * if `unit.kind` is `Integer`, the last element can be shorter, i.e., `{ i64, i64, i32 }`
    ///   for 64-bit integers with a total size of 20 bytes. When the argument is actually passed,
    ///   this size will be rounded up to the nearest multiple of `unit.size`.
    pub total: Size,

    /// Force the use of an array, even if there is only a single element.
    pub force_array: bool,
}

impl From<Reg> for Uniform {
    fn from(unit: Reg) -> Uniform {
        Uniform { unit, total: unit.size, force_array: false }
    }
}

impl Uniform {
    pub fn align<C: HasDataLayout>(&self, cx: &C) -> Align {
        self.unit.align(cx)
    }
}

/// Describes the type used for `PassMode::Cast`.
///
/// Passing arguments in this mode works as follows: the registers in the `prefix` (the ones that
/// are `Some`) get laid out one after the other (using `repr(C)` layout rules). Then the
/// `rest.unit` register type gets repeated often enough to cover `rest.size`. This describes the
/// actual type used for the call; the Rust type of the argument is then transmuted to this ABI type
/// (and all data in the padding between the registers is dropped).
#[derive(Clone, PartialEq, Eq, Hash, Debug, HashStable_Generic)]
pub struct CastTarget {
    pub prefix: [Option<Reg>; 8],
    pub rest: Uniform,
    pub attrs: ArgAttributes,
}

impl From<Reg> for CastTarget {
    fn from(unit: Reg) -> CastTarget {
        CastTarget::from(Uniform::from(unit))
    }
}

impl From<Uniform> for CastTarget {
    fn from(uniform: Uniform) -> CastTarget {
        CastTarget {
            prefix: [None; 8],
            rest: uniform,
            attrs: ArgAttributes {
                regular: ArgAttribute::default(),
                arg_ext: ArgExtension::None,
                pointee_size: Size::ZERO,
                pointee_align: None,
            },
        }
    }
}

impl CastTarget {
    pub fn pair(a: Reg, b: Reg) -> CastTarget {
        CastTarget {
            prefix: [Some(a), None, None, None, None, None, None, None],
            rest: Uniform::from(b),
            attrs: ArgAttributes {
                regular: ArgAttribute::default(),
                arg_ext: ArgExtension::None,
                pointee_size: Size::ZERO,
                pointee_align: None,
            },
        }
    }

    pub fn size<C: HasDataLayout>(&self, _cx: &C) -> Size {
        // Prefix arguments are passed in specific designated registers
        let prefix_size = self
            .prefix
            .iter()
            .filter_map(|x| x.map(|reg| reg.size))
            .fold(Size::ZERO, |acc, size| acc + size);
        // Remaining arguments are passed in chunks of the unit size
        let rest_size =
            self.rest.unit.size * self.rest.total.bytes().div_ceil(self.rest.unit.size.bytes());

        prefix_size + rest_size
    }

    pub fn align<C: HasDataLayout>(&self, cx: &C) -> Align {
        self.prefix
            .iter()
            .filter_map(|x| x.map(|reg| reg.align(cx)))
            .fold(cx.data_layout().aggregate_align.abi.max(self.rest.align(cx)), |acc, align| {
                acc.max(align)
            })
    }

    /// Checks if these two `CastTarget` are equal enough to be considered "the same for all
    /// function call ABIs".
    pub fn eq_abi(&self, other: &Self) -> bool {
        let CastTarget { prefix: prefix_l, rest: rest_l, attrs: attrs_l } = self;
        let CastTarget { prefix: prefix_r, rest: rest_r, attrs: attrs_r } = other;
        prefix_l == prefix_r && rest_l == rest_r && attrs_l.eq_abi(attrs_r)
    }
}

/// Return value from the `homogeneous_aggregate` test function.
#[derive(Copy, Clone, Debug)]
pub enum HomogeneousAggregate {
    /// Yes, all the "leaf fields" of this struct are passed in the
    /// same way (specified in the `Reg` value).
    Homogeneous(Reg),

    /// There are no leaf fields at all.
    NoData,
}

/// Error from the `homogeneous_aggregate` test function, indicating
/// there are distinct leaf fields passed in different ways,
/// or this is uninhabited.
#[derive(Copy, Clone, Debug)]
pub struct Heterogeneous;

impl HomogeneousAggregate {
    /// If this is a homogeneous aggregate, returns the homogeneous
    /// unit, else `None`.
    pub fn unit(self) -> Option<Reg> {
        match self {
            HomogeneousAggregate::Homogeneous(reg) => Some(reg),
            HomogeneousAggregate::NoData => None,
        }
    }

    /// Try to combine two `HomogeneousAggregate`s, e.g. from two fields in
    /// the same `struct`. Only succeeds if only one of them has any data,
    /// or both units are identical.
    fn merge(self, other: HomogeneousAggregate) -> Result<HomogeneousAggregate, Heterogeneous> {
        match (self, other) {
            (x, HomogeneousAggregate::NoData) | (HomogeneousAggregate::NoData, x) => Ok(x),

            (HomogeneousAggregate::Homogeneous(a), HomogeneousAggregate::Homogeneous(b)) => {
                if a != b {
                    return Err(Heterogeneous);
                }
                Ok(self)
            }
        }
    }
}

impl<'a, Ty> TyAndLayout<'a, Ty> {
    /// Returns `true` if this is an aggregate type (including a ScalarPair!)
    fn is_aggregate(&self) -> bool {
        match self.abi {
            Abi::Uninhabited | Abi::Scalar(_) | Abi::Vector { .. } => false,
            Abi::ScalarPair(..) | Abi::Aggregate { .. } => true,
        }
    }

    /// Returns `Homogeneous` if this layout is an aggregate containing fields of
    /// only a single type (e.g., `(u32, u32)`). Such aggregates are often
    /// special-cased in ABIs.
    ///
    /// Note: We generally ignore 1-ZST fields when computing this value (see #56877).
    ///
    /// This is public so that it can be used in unit tests, but
    /// should generally only be relevant to the ABI details of
    /// specific targets.
    pub fn homogeneous_aggregate<C>(&self, cx: &C) -> Result<HomogeneousAggregate, Heterogeneous>
    where
        Ty: TyAbiInterface<'a, C> + Copy,
    {
        match self.abi {
            Abi::Uninhabited => Err(Heterogeneous),

            // The primitive for this algorithm.
            Abi::Scalar(scalar) => {
                let kind = match scalar.primitive() {
                    abi::Int(..) | abi::Pointer(_) => RegKind::Integer,
                    abi::F16 | abi::F32 | abi::F64 | abi::F128 => RegKind::Float,
                };
                Ok(HomogeneousAggregate::Homogeneous(Reg { kind, size: self.size }))
            }

            Abi::Vector { .. } => {
                assert!(!self.is_zst());
                Ok(HomogeneousAggregate::Homogeneous(Reg {
                    kind: RegKind::Vector,
                    size: self.size,
                }))
            }

            Abi::ScalarPair(..) | Abi::Aggregate { sized: true } => {
                // Helper for computing `homogeneous_aggregate`, allowing a custom
                // starting offset (used below for handling variants).
                let from_fields_at =
                    |layout: Self,
                     start: Size|
                     -> Result<(HomogeneousAggregate, Size), Heterogeneous> {
                        let is_union = match layout.fields {
                            FieldsShape::Primitive => {
                                unreachable!("aggregates can't have `FieldsShape::Primitive`")
                            }
                            FieldsShape::Array { count, .. } => {
                                assert_eq!(start, Size::ZERO);

                                let result = if count > 0 {
                                    layout.field(cx, 0).homogeneous_aggregate(cx)?
                                } else {
                                    HomogeneousAggregate::NoData
                                };
                                return Ok((result, layout.size));
                            }
                            FieldsShape::Union(_) => true,
                            FieldsShape::Arbitrary { .. } => false,
                        };

                        let mut result = HomogeneousAggregate::NoData;
                        let mut total = start;

                        for i in 0..layout.fields.count() {
                            let field = layout.field(cx, i);
                            if field.is_1zst() {
                                // No data here and no impact on layout, can be ignored.
                                // (We might be able to also ignore all aligned ZST but that's less clear.)
                                continue;
                            }

                            if !is_union && total != layout.fields.offset(i) {
                                // This field isn't just after the previous one we considered, abort.
                                return Err(Heterogeneous);
                            }

                            result = result.merge(field.homogeneous_aggregate(cx)?)?;

                            // Keep track of the offset (without padding).
                            let size = field.size;
                            if is_union {
                                total = total.max(size);
                            } else {
                                total += size;
                            }
                        }

                        Ok((result, total))
                    };

                let (mut result, mut total) = from_fields_at(*self, Size::ZERO)?;

                match &self.variants {
                    abi::Variants::Single { .. } => {}
                    abi::Variants::Multiple { variants, .. } => {
                        // Treat enum variants like union members.
                        // HACK(eddyb) pretend the `enum` field (discriminant)
                        // is at the start of every variant (otherwise the gap
                        // at the start of all variants would disqualify them).
                        //
                        // NB: for all tagged `enum`s (which include all non-C-like
                        // `enum`s with defined FFI representation), this will
                        // match the homogeneous computation on the equivalent
                        // `struct { tag; union { variant1; ... } }` and/or
                        // `union { struct { tag; variant1; } ... }`
                        // (the offsets of variant fields should be identical
                        // between the two for either to be a homogeneous aggregate).
                        let variant_start = total;
                        for variant_idx in variants.indices() {
                            let (variant_result, variant_total) =
                                from_fields_at(self.for_variant(cx, variant_idx), variant_start)?;

                            result = result.merge(variant_result)?;
                            total = total.max(variant_total);
                        }
                    }
                }

                // There needs to be no padding.
                if total != self.size {
                    Err(Heterogeneous)
                } else {
                    match result {
                        HomogeneousAggregate::Homogeneous(_) => {
                            assert_ne!(total, Size::ZERO);
                        }
                        HomogeneousAggregate::NoData => {
                            assert_eq!(total, Size::ZERO);
                        }
                    }
                    Ok(result)
                }
            }
            Abi::Aggregate { sized: false } => Err(Heterogeneous),
        }
    }
}

/// Information about how to pass an argument to,
/// or return a value from, a function, under some ABI.
#[derive(Clone, PartialEq, Eq, Hash, HashStable_Generic)]
pub struct ArgAbi<'a, Ty> {
    pub layout: TyAndLayout<'a, Ty>,
    pub mode: PassMode,
}

// Needs to be a custom impl because of the bounds on the `TyAndLayout` debug impl.
impl<'a, Ty: fmt::Display> fmt::Debug for ArgAbi<'a, Ty> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let ArgAbi { layout, mode } = self;
        f.debug_struct("ArgAbi").field("layout", layout).field("mode", mode).finish()
    }
}

impl<'a, Ty> ArgAbi<'a, Ty> {
    /// This defines the "default ABI" for that type, that is then later adjusted in `fn_abi_adjust_for_abi`.
    pub fn new(
        cx: &impl HasDataLayout,
        layout: TyAndLayout<'a, Ty>,
        scalar_attrs: impl Fn(&TyAndLayout<'a, Ty>, abi::Scalar, Size) -> ArgAttributes,
    ) -> Self {
        let mode = match layout.abi {
            Abi::Uninhabited => PassMode::Ignore,
            Abi::Scalar(scalar) => PassMode::Direct(scalar_attrs(&layout, scalar, Size::ZERO)),
            Abi::ScalarPair(a, b) => PassMode::Pair(
                scalar_attrs(&layout, a, Size::ZERO),
                scalar_attrs(&layout, b, a.size(cx).align_to(b.align(cx).abi)),
            ),
            Abi::Vector { .. } => PassMode::Direct(ArgAttributes::new()),
            Abi::Aggregate { .. } => Self::indirect_pass_mode(&layout),
        };
        ArgAbi { layout, mode }
    }

    fn indirect_pass_mode(layout: &TyAndLayout<'a, Ty>) -> PassMode {
        let mut attrs = ArgAttributes::new();

        // For non-immediate arguments the callee gets its own copy of
        // the value on the stack, so there are no aliases. It's also
        // program-invisible so can't possibly capture
        attrs
            .set(ArgAttribute::NoAlias)
            .set(ArgAttribute::NoCapture)
            .set(ArgAttribute::NonNull)
            .set(ArgAttribute::NoUndef);
        attrs.pointee_size = layout.size;
        attrs.pointee_align = Some(layout.align.abi);

        let meta_attrs = layout.is_unsized().then_some(ArgAttributes::new());

        PassMode::Indirect { attrs, meta_attrs, on_stack: false }
    }

    /// Pass this argument directly instead. Should NOT be used!
    /// Only exists because of past ABI mistakes that will take time to fix
    /// (see <https://github.com/rust-lang/rust/issues/115666>).
    pub fn make_direct_deprecated(&mut self) {
        match self.mode {
            PassMode::Indirect { .. } => {
                self.mode = PassMode::Direct(ArgAttributes::new());
            }
            PassMode::Ignore | PassMode::Direct(_) | PassMode::Pair(_, _) => return, // already direct
            _ => panic!("Tried to make {:?} direct", self.mode),
        }
    }

    /// Pass this argument indirectly, by passing a (thin or fat) pointer to the argument instead.
    /// This is valid for both sized and unsized arguments.
    pub fn make_indirect(&mut self) {
        match self.mode {
            PassMode::Direct(_) | PassMode::Pair(_, _) => {
                self.mode = Self::indirect_pass_mode(&self.layout);
            }
            PassMode::Indirect { attrs: _, meta_attrs: _, on_stack: false } => {
                // already indirect
                return;
            }
            _ => panic!("Tried to make {:?} indirect", self.mode),
        }
    }

    /// Pass this argument indirectly, by placing it at a fixed stack offset.
    /// This corresponds to the `byval` LLVM argument attribute.
    /// This is only valid for sized arguments.
    ///
    /// `byval_align` specifies the alignment of the `byval` stack slot, which does not need to
    /// correspond to the type's alignment. This will be `Some` if the target's ABI specifies that
    /// stack slots used for arguments passed by-value have specific alignment requirements which
    /// differ from the alignment used in other situations.
    ///
    /// If `None`, the type's alignment is used.
    ///
    /// If the resulting alignment differs from the type's alignment,
    /// the argument will be copied to an alloca with sufficient alignment,
    /// either in the caller (if the type's alignment is lower than the byval alignment)
    /// or in the callee (if the type's alignment is higher than the byval alignment),
    /// to ensure that Rust code never sees an underaligned pointer.
    pub fn make_indirect_byval(&mut self, byval_align: Option<Align>) {
        assert!(!self.layout.is_unsized(), "used byval ABI for unsized layout");
        self.make_indirect();
        match self.mode {
            PassMode::Indirect { ref mut attrs, meta_attrs: _, ref mut on_stack } => {
                *on_stack = true;

                // Some platforms, like 32-bit x86, change the alignment of the type when passing
                // `byval`. Account for that.
                if let Some(byval_align) = byval_align {
                    // On all targets with byval align this is currently true, so let's assert it.
                    debug_assert!(byval_align >= Align::from_bytes(4).unwrap());
                    attrs.pointee_align = Some(byval_align);
                }
            }
            _ => unreachable!(),
        }
    }

    pub fn extend_integer_width_to(&mut self, bits: u64) {
        // Only integers have signedness
        if let Abi::Scalar(scalar) = self.layout.abi {
            if let abi::Int(i, signed) = scalar.primitive() {
                if i.size().bits() < bits {
                    if let PassMode::Direct(ref mut attrs) = self.mode {
                        if signed {
                            attrs.ext(ArgExtension::Sext)
                        } else {
                            attrs.ext(ArgExtension::Zext)
                        };
                    }
                }
            }
        }
    }

    pub fn cast_to<T: Into<CastTarget>>(&mut self, target: T) {
        self.mode = PassMode::Cast { cast: Box::new(target.into()), pad_i32: false };
    }

    pub fn cast_to_and_pad_i32<T: Into<CastTarget>>(&mut self, target: T, pad_i32: bool) {
        self.mode = PassMode::Cast { cast: Box::new(target.into()), pad_i32 };
    }

    pub fn is_indirect(&self) -> bool {
        matches!(self.mode, PassMode::Indirect { .. })
    }

    pub fn is_sized_indirect(&self) -> bool {
        matches!(self.mode, PassMode::Indirect { attrs: _, meta_attrs: None, on_stack: _ })
    }

    pub fn is_unsized_indirect(&self) -> bool {
        matches!(self.mode, PassMode::Indirect { attrs: _, meta_attrs: Some(_), on_stack: _ })
    }

    pub fn is_ignore(&self) -> bool {
        matches!(self.mode, PassMode::Ignore)
    }

    /// Checks if these two `ArgAbi` are equal enough to be considered "the same for all
    /// function call ABIs".
    pub fn eq_abi(&self, other: &Self) -> bool {
        // Ideally we'd just compare the `mode`, but that is not enough -- for some modes LLVM will look
        // at the type.
        self.layout.eq_abi(&other.layout) && self.mode.eq_abi(&other.mode)
    }
}

#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, HashStable_Generic)]
pub enum Conv {
    // General language calling conventions, for which every target
    // should have its own backend (e.g. LLVM) support.
    C,
    Rust,

    Cold,
    PreserveMost,
    PreserveAll,

    // Target-specific calling conventions.
    ArmAapcs,
    CCmseNonSecureCall,

    Msp430Intr,

    PtxKernel,

    X86Fastcall,
    X86Intr,
    X86Stdcall,
    X86ThisCall,
    X86VectorCall,

    X86_64SysV,
    X86_64Win64,

    AvrInterrupt,
    AvrNonBlockingInterrupt,

    RiscvInterrupt { kind: RiscvInterruptKind },
}

#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, HashStable_Generic)]
pub enum RiscvInterruptKind {
    Machine,
    Supervisor,
}

impl RiscvInterruptKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Machine => "machine",
            Self::Supervisor => "supervisor",
        }
    }
}

/// Metadata describing how the arguments to a native function
/// should be passed in order to respect the native ABI.
///
/// I will do my best to describe this structure, but these
/// comments are reverse-engineered and may be inaccurate. -NDM
#[derive(Clone, PartialEq, Eq, Hash, HashStable_Generic)]
pub struct FnAbi<'a, Ty> {
    /// The LLVM types of each argument.
    pub args: Box<[ArgAbi<'a, Ty>]>,

    /// LLVM return type.
    pub ret: ArgAbi<'a, Ty>,

    pub c_variadic: bool,

    /// The count of non-variadic arguments.
    ///
    /// Should only be different from args.len() when c_variadic is true.
    /// This can be used to know whether an argument is variadic or not.
    pub fixed_count: u32,

    pub conv: Conv,

    pub can_unwind: bool,
}

// Needs to be a custom impl because of the bounds on the `TyAndLayout` debug impl.
impl<'a, Ty: fmt::Display> fmt::Debug for FnAbi<'a, Ty> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let FnAbi { args, ret, c_variadic, fixed_count, conv, can_unwind } = self;
        f.debug_struct("FnAbi")
            .field("args", args)
            .field("ret", ret)
            .field("c_variadic", c_variadic)
            .field("fixed_count", fixed_count)
            .field("conv", conv)
            .field("can_unwind", can_unwind)
            .finish()
    }
}

/// Error produced by attempting to adjust a `FnAbi`, for a "foreign" ABI.
#[derive(Copy, Clone, Debug, HashStable_Generic)]
pub enum AdjustForForeignAbiError {
    /// Target architecture doesn't support "foreign" (i.e. non-Rust) ABIs.
    Unsupported { arch: Symbol, abi: spec::abi::Abi },
}

impl<'a, Ty> FnAbi<'a, Ty> {
    pub fn adjust_for_foreign_abi<C>(
        &mut self,
        cx: &C,
        abi: spec::abi::Abi,
    ) -> Result<(), AdjustForForeignAbiError>
    where
        Ty: TyAbiInterface<'a, C> + Copy,
        C: HasDataLayout + HasTargetSpec,
    {
        if abi == spec::abi::Abi::X86Interrupt {
            if let Some(arg) = self.args.first_mut() {
                // FIXME(pcwalton): This probably should use the x86 `byval` ABI...
                arg.make_indirect_byval(None);
            }
            return Ok(());
        }

        match &cx.target_spec().arch[..] {
            "x86" => {
                let flavor = if let spec::abi::Abi::Fastcall { .. }
                | spec::abi::Abi::Vectorcall { .. } = abi
                {
                    x86::Flavor::FastcallOrVectorcall
                } else {
                    x86::Flavor::General
                };
                x86::compute_abi_info(cx, self, flavor);
            }
            "x86_64" => match abi {
                spec::abi::Abi::SysV64 { .. } => x86_64::compute_abi_info(cx, self),
                spec::abi::Abi::Win64 { .. } => x86_win64::compute_abi_info(self),
                _ => {
                    if cx.target_spec().is_like_windows {
                        x86_win64::compute_abi_info(self)
                    } else {
                        x86_64::compute_abi_info(cx, self)
                    }
                }
            },
            "aarch64" | "arm64ec" => {
                let kind = if cx.target_spec().is_like_osx {
                    aarch64::AbiKind::DarwinPCS
                } else if cx.target_spec().is_like_windows {
                    aarch64::AbiKind::Win64
                } else {
                    aarch64::AbiKind::AAPCS
                };
                aarch64::compute_abi_info(cx, self, kind)
            }
            "amdgpu" => amdgpu::compute_abi_info(cx, self),
            "arm" => arm::compute_abi_info(cx, self),
            "avr" => avr::compute_abi_info(self),
            "loongarch64" => loongarch::compute_abi_info(cx, self),
            "m68k" => m68k::compute_abi_info(self),
            "csky" => csky::compute_abi_info(self),
            "mips" | "mips32r6" => mips::compute_abi_info(cx, self),
            "mips64" | "mips64r6" => mips64::compute_abi_info(cx, self),
            "powerpc" => powerpc::compute_abi_info(self),
            "powerpc64" => powerpc64::compute_abi_info(cx, self),
            "s390x" => s390x::compute_abi_info(cx, self),
            "msp430" => msp430::compute_abi_info(self),
            "sparc" => sparc::compute_abi_info(cx, self),
            "sparc64" => sparc64::compute_abi_info(cx, self),
            "nvptx64" => {
                if cx.target_spec().adjust_abi(abi, self.c_variadic) == spec::abi::Abi::PtxKernel {
                    nvptx64::compute_ptx_kernel_abi_info(cx, self)
                } else {
                    nvptx64::compute_abi_info(self)
                }
            }
            "hexagon" => hexagon::compute_abi_info(self),
            "riscv32" | "riscv64" => riscv::compute_abi_info(cx, self),
            "wasm32" | "wasm64" => {
                if cx.target_spec().adjust_abi(abi, self.c_variadic) == spec::abi::Abi::Wasm {
                    wasm::compute_wasm_abi_info(self)
                } else {
                    wasm::compute_c_abi_info(cx, self)
                }
            }
            "bpf" => bpf::compute_abi_info(self),
            arch => {
                return Err(AdjustForForeignAbiError::Unsupported {
                    arch: Symbol::intern(arch),
                    abi,
                });
            }
        }

        Ok(())
    }
}

impl FromStr for Conv {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "C" => Ok(Conv::C),
            "Rust" => Ok(Conv::Rust),
            "RustCold" => Ok(Conv::Rust),
            "ArmAapcs" => Ok(Conv::ArmAapcs),
            "CCmseNonSecureCall" => Ok(Conv::CCmseNonSecureCall),
            "Msp430Intr" => Ok(Conv::Msp430Intr),
            "PtxKernel" => Ok(Conv::PtxKernel),
            "X86Fastcall" => Ok(Conv::X86Fastcall),
            "X86Intr" => Ok(Conv::X86Intr),
            "X86Stdcall" => Ok(Conv::X86Stdcall),
            "X86ThisCall" => Ok(Conv::X86ThisCall),
            "X86VectorCall" => Ok(Conv::X86VectorCall),
            "X86_64SysV" => Ok(Conv::X86_64SysV),
            "X86_64Win64" => Ok(Conv::X86_64Win64),
            "AvrInterrupt" => Ok(Conv::AvrInterrupt),
            "AvrNonBlockingInterrupt" => Ok(Conv::AvrNonBlockingInterrupt),
            "RiscvInterrupt(machine)" => {
                Ok(Conv::RiscvInterrupt { kind: RiscvInterruptKind::Machine })
            }
            "RiscvInterrupt(supervisor)" => {
                Ok(Conv::RiscvInterrupt { kind: RiscvInterruptKind::Supervisor })
            }
            _ => Err(format!("'{s}' is not a valid value for entry function call convention.")),
        }
    }
}

// Some types are used a lot. Make sure they don't unintentionally get bigger.
#[cfg(all(any(target_arch = "x86_64", target_arch = "aarch64"), target_pointer_width = "64"))]
mod size_asserts {
    use super::*;
    use rustc_data_structures::static_assert_size;
    // tidy-alphabetical-start
    static_assert_size!(ArgAbi<'_, usize>, 56);
    static_assert_size!(FnAbi<'_, usize>, 80);
    // tidy-alphabetical-end
}
