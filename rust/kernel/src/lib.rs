//! A minimal category/functor conformance kernel.
//!
//! This is the bottom layer (L0) of Theseus: finite categories, functors between
//! them, and the one law we care about for architectural conformance — a functor
//! sends every source morphism to a target morphism with matching endpoints.
//!
//! That law, plus object and morphism mappings, is all the structural checks
//! Theseus needs: required edges present, forbidden edges absent, dependency
//! direction respected. Identity morphisms are synthesized per object and mapped
//! automatically, so a caller maps only its named objects and morphisms.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
};

/// A label naming an object (a node) in a category.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ObjectLabel(String);

/// A label naming a morphism (an edge) in a category.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MorphismLabel(String);

macro_rules! string_newtype {
    ($name:ident) => {
        impl $name {
            pub fn new(value: impl Into<String>) -> Self {
                Self(value.into())
            }
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }
        impl From<&str> for $name {
            fn from(value: &str) -> Self {
                Self(value.to_string())
            }
        }
        impl From<String> for $name {
            fn from(value: String) -> Self {
                Self(value)
            }
        }
        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

string_newtype!(ObjectLabel);
string_newtype!(MorphismLabel);

/// A single arrow: a labelled edge from `source` to `target`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Morphism {
    label: MorphismLabel,
    source: ObjectLabel,
    target: ObjectLabel,
    identity: bool,
}

impl Morphism {
    pub fn label(&self) -> &MorphismLabel {
        &self.label
    }
    pub fn source(&self) -> &ObjectLabel {
        &self.source
    }
    pub fn target(&self) -> &ObjectLabel {
        &self.target
    }
    pub fn is_identity(&self) -> bool {
        self.identity
    }
}

/// A finite category: a set of objects and a set of morphisms between them,
/// with one synthesized identity morphism per object.
#[derive(Debug, Clone)]
pub struct Category {
    id: String,
    name: String,
    objects: BTreeSet<ObjectLabel>,
    morphisms: BTreeMap<MorphismLabel, Morphism>,
}

impl Category {
    pub fn id(&self) -> &str {
        &self.id
    }
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn objects(&self) -> &BTreeSet<ObjectLabel> {
        &self.objects
    }
    pub fn morphisms(&self) -> &BTreeMap<MorphismLabel, Morphism> {
        &self.morphisms
    }
    pub fn morphism(&self, label: &MorphismLabel) -> Option<&Morphism> {
        self.morphisms.get(label)
    }

    /// The canonical identity label for an object, `id_<object>`.
    pub fn identity_label(object: &ObjectLabel) -> MorphismLabel {
        MorphismLabel::new(format!("id_{}", object.as_str()))
    }

    /// Does a direct, non-identity edge exist from `source` to `target`?
    ///
    /// Backs the "required edge present" and "forbidden edge absent" checks.
    pub fn has_direct_edge(&self, source: &str, target: &str) -> bool {
        self.morphisms
            .values()
            .any(|m| !m.identity && m.source.as_str() == source && m.target.as_str() == target)
    }
}

/// Fluent builder for a [`Category`]. Identities are added automatically on
/// [`build`](CategoryBuilder::build).
#[derive(Debug, Default)]
pub struct CategoryBuilder {
    id: String,
    name: String,
    objects: BTreeSet<ObjectLabel>,
    edges: Vec<(MorphismLabel, ObjectLabel, ObjectLabel)>,
}

impl CategoryBuilder {
    pub fn new(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            ..Self::default()
        }
    }

    pub fn object(mut self, name: &str) -> Self {
        self.objects.insert(ObjectLabel::from(name));
        self
    }

    pub fn objects(mut self, names: &[&str]) -> Self {
        for name in names {
            self.objects.insert(ObjectLabel::from(*name));
        }
        self
    }

    pub fn morphism(mut self, label: &str, source: &str, target: &str) -> Self {
        self.edges.push((
            MorphismLabel::from(label),
            ObjectLabel::from(source),
            ObjectLabel::from(target),
        ));
        self
    }

    pub fn morphisms(mut self, edges: &[(&str, &str, &str)]) -> Self {
        for (label, source, target) in edges {
            self = self.morphism(label, source, target);
        }
        self
    }

    pub fn build(self) -> Result<Category, KernelError> {
        let mut morphisms = BTreeMap::new();
        for object in &self.objects {
            let label = Category::identity_label(object);
            morphisms.insert(
                label.clone(),
                Morphism {
                    label,
                    source: object.clone(),
                    target: object.clone(),
                    identity: true,
                },
            );
        }
        for (label, source, target) in self.edges {
            for endpoint in [&source, &target] {
                if !self.objects.contains(endpoint) {
                    return Err(KernelError::UnknownObject {
                        morphism: label.to_string(),
                        object: endpoint.to_string(),
                    });
                }
            }
            if morphisms
                .insert(
                    label.clone(),
                    Morphism {
                        label: label.clone(),
                        source,
                        target,
                        identity: false,
                    },
                )
                .is_some()
            {
                return Err(KernelError::DuplicateMorphism(label.to_string()));
            }
        }
        Ok(Category {
            id: self.id,
            name: self.name,
            objects: self.objects,
            morphisms,
        })
    }
}

/// A functor between two categories: a mapping of objects to objects and
/// (non-identity) morphisms to morphisms. Identity morphisms are mapped
/// implicitly: `id_X` maps to `id_{F(X)}`.
#[derive(Debug, Clone)]
pub struct Functor {
    id: String,
    name: String,
    object_map: BTreeMap<ObjectLabel, ObjectLabel>,
    morphism_map: BTreeMap<MorphismLabel, MorphismLabel>,
}

impl Functor {
    pub fn id(&self) -> &str {
        &self.id
    }
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn object_map(&self) -> &BTreeMap<ObjectLabel, ObjectLabel> {
        &self.object_map
    }
    pub fn morphism_map(&self) -> &BTreeMap<MorphismLabel, MorphismLabel> {
        &self.morphism_map
    }

    /// Verify the functor laws against the two categories it claims to relate.
    ///
    /// Every object of `source` must be mapped to an object of `target`, and
    /// every non-identity morphism of `source` must be mapped to a morphism of
    /// `target` whose endpoints are the images of the original endpoints.
    /// Identity preservation is checked structurally from the object map.
    pub fn verify(&self, source: &Category, target: &Category) -> Result<(), FunctorError> {
        for object in &source.objects {
            let image = self
                .object_map
                .get(object)
                .ok_or_else(|| FunctorError::ObjectNotMapped(object.to_string()))?;
            if !target.objects.contains(image) {
                return Err(FunctorError::ImageObjectMissing {
                    object: object.to_string(),
                    image: image.to_string(),
                });
            }
        }

        for (label, morphism) in &source.morphisms {
            if morphism.identity {
                continue;
            }
            let image_label = self
                .morphism_map
                .get(label)
                .ok_or_else(|| FunctorError::MorphismNotMapped(label.to_string()))?;
            let image = target.morphisms.get(image_label).ok_or_else(|| {
                FunctorError::ImageMorphismMissing {
                    morphism: label.to_string(),
                    image: image_label.to_string(),
                }
            })?;

            let expected_source = self.image_of(&morphism.source)?;
            let expected_target = self.image_of(&morphism.target)?;
            if image.source != expected_source || image.target != expected_target {
                return Err(FunctorError::EndpointMismatch {
                    morphism: label.to_string(),
                    expected: format!("{expected_source} -> {expected_target}"),
                    found: format!("{} -> {}", image.source, image.target),
                });
            }
        }
        Ok(())
    }

    fn image_of(&self, object: &ObjectLabel) -> Result<ObjectLabel, FunctorError> {
        self.object_map
            .get(object)
            .cloned()
            .ok_or_else(|| FunctorError::ObjectNotMapped(object.to_string()))
    }
}

/// Fluent builder for a [`Functor`].
#[derive(Debug, Default)]
pub struct FunctorBuilder {
    id: String,
    name: String,
    object_map: BTreeMap<ObjectLabel, ObjectLabel>,
    morphism_map: BTreeMap<MorphismLabel, MorphismLabel>,
}

impl FunctorBuilder {
    pub fn new(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            ..Self::default()
        }
    }

    pub fn map_object(mut self, source: &str, target: &str) -> Self {
        self.object_map
            .insert(ObjectLabel::from(source), ObjectLabel::from(target));
        self
    }

    pub fn map_objects(mut self, pairs: &[(&str, &str)]) -> Self {
        for (source, target) in pairs {
            self = self.map_object(source, target);
        }
        self
    }

    pub fn map_morphism(mut self, source: &str, target: &str) -> Self {
        self.morphism_map
            .insert(MorphismLabel::from(source), MorphismLabel::from(target));
        self
    }

    pub fn map_morphisms(mut self, pairs: &[(&str, &str)]) -> Self {
        for (source, target) in pairs {
            self = self.map_morphism(source, target);
        }
        self
    }

    pub fn build(self) -> Functor {
        Functor {
            id: self.id,
            name: self.name,
            object_map: self.object_map,
            morphism_map: self.morphism_map,
        }
    }
}

/// Errors raised while building a [`Category`].
#[derive(Debug, thiserror::Error)]
pub enum KernelError {
    #[error("morphism `{morphism}` references unknown object `{object}`")]
    UnknownObject { morphism: String, object: String },
    #[error("duplicate morphism label `{0}`")]
    DuplicateMorphism(String),
}

/// Errors raised while verifying a [`Functor`].
#[derive(Debug, thiserror::Error)]
pub enum FunctorError {
    #[error("object `{0}` in the source category is not mapped")]
    ObjectNotMapped(String),
    #[error("object `{object}` maps to `{image}`, which is not in the target category")]
    ImageObjectMissing { object: String, image: String },
    #[error("morphism `{0}` in the source category is not mapped")]
    MorphismNotMapped(String),
    #[error("morphism `{morphism}` maps to `{image}`, which is not in the target category")]
    ImageMorphismMissing { morphism: String, image: String },
    #[error("functor breaks endpoints of `{morphism}`: expected {expected}, image goes {found}")]
    EndpointMismatch {
        morphism: String,
        expected: String,
        found: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn diamond_spec() -> Category {
        CategoryBuilder::new("Spec", "spec")
            .objects(&["A", "B"])
            .morphism("f", "A", "B")
            .build()
            .unwrap()
    }

    #[test]
    fn build_adds_identities() {
        let c = diamond_spec();
        assert!(
            c.morphism(&MorphismLabel::from("id_A"))
                .unwrap()
                .is_identity()
        );
        assert!(c.has_direct_edge("A", "B"));
        assert!(!c.has_direct_edge("B", "A"));
    }

    #[test]
    fn unknown_endpoint_is_rejected() {
        let err = CategoryBuilder::new("Bad", "bad")
            .object("A")
            .morphism("f", "A", "B")
            .build()
            .unwrap_err();
        assert!(matches!(err, KernelError::UnknownObject { .. }));
    }

    #[test]
    fn faithful_functor_verifies() {
        let spec = diamond_spec();
        let impl_cat = CategoryBuilder::new("Impl", "impl")
            .objects(&["X", "Y"])
            .morphism("g", "X", "Y")
            .build()
            .unwrap();
        let functor = FunctorBuilder::new("F", "f")
            .map_objects(&[("A", "X"), ("B", "Y")])
            .map_morphism("f", "g")
            .build();
        assert!(functor.verify(&spec, &impl_cat).is_ok());
    }

    #[test]
    fn endpoint_violation_is_detected() {
        let spec = diamond_spec();
        // `g` runs backwards relative to the image of `f`.
        let impl_cat = CategoryBuilder::new("Impl", "impl")
            .objects(&["X", "Y"])
            .morphism("g", "Y", "X")
            .build()
            .unwrap();
        let functor = FunctorBuilder::new("F", "f")
            .map_objects(&[("A", "X"), ("B", "Y")])
            .map_morphism("f", "g")
            .build();
        assert!(matches!(
            functor.verify(&spec, &impl_cat),
            Err(FunctorError::EndpointMismatch { .. })
        ));
    }

    #[test]
    fn missing_morphism_mapping_is_detected() {
        let spec = diamond_spec();
        let impl_cat = CategoryBuilder::new("Impl", "impl")
            .objects(&["X", "Y"])
            .build()
            .unwrap();
        let functor = FunctorBuilder::new("F", "f")
            .map_objects(&[("A", "X"), ("B", "Y")])
            .build();
        assert!(matches!(
            functor.verify(&spec, &impl_cat),
            Err(FunctorError::MorphismNotMapped(_))
        ));
    }
}
