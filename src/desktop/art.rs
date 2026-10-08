//! Curated CC0 GLB prefabs and mipmapped terrain arrays, loaded once per app.
use super::{ModelKind, UiState};
use anyhow::{Context, Result, bail, ensure};
use bevy::{
    asset::{LoadState, RecursiveDependencyLoadState},
    gltf::{Gltf, GltfMaterial, GltfMesh, GltfNode},
    image::{
        ImageAddressMode, ImageFilterMode, ImageLoaderSettings, ImageSampler,
        ImageSamplerDescriptor,
    },
    mesh::VertexAttributeValues,
    prelude::*,
    render::render_resource::{TextureFormat, TextureViewDescriptor, TextureViewDimension},
};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

#[derive(Deserialize)]
struct Manifest {
    models: Vec<ModelSpec>,
    files: Vec<FileSpec>,
}
#[derive(Deserialize)]
struct ModelSpec {
    path: String,
    kind: ModelKind,
}
#[derive(Deserialize)]
struct FileSpec {
    path: String,
    sha256: String,
}
fn manifest() -> Manifest {
    serde_json::from_str(include_str!("../../assets/art/manifest.json"))
        .expect("Curated art manifest")
}

pub fn asset_root() -> Result<PathBuf> {
    let executable = std::env::current_exe()?;
    let parent = executable
        .parent()
        .context("Executable directory missing")?;
    let candidates = [
        parent.join("../Resources/assets"),
        parent.join("assets"),
        Path::new(env!("CARGO_MANIFEST_DIR")).join("assets"),
    ];
    let root = candidates.into_iter().find(|p| p.join("art/manifest.json").exists())
        .context("Art assets missing. Restore assets with git lfs pull, or reinstall the complete native package.")?;
    validate_root(&root)?;
    Ok(root)
}
fn validate_root(root: &Path) -> Result<()> {
    for file in manifest().files {
        let path = root.join(&file.path);
        let bytes = std::fs::read(&path)
            .with_context(|| format!("Missing art {}. Run git lfs pull.", path.display()))?;
        ensure!(
            format!("{:x}", Sha256::digest(&bytes)) == file.sha256,
            "Invalid or unhydrated art {}. Run git lfs pull; primitive replacements are not supplied.",
            path.display()
        );
    }
    Ok(())
}

pub struct ModelPart {
    pub mesh: Handle<Mesh>,
    pub material: Handle<StandardMaterial>,
}
pub struct Prefab {
    pub parts: Vec<ModelPart>,
}
struct PendingModel {
    spec: ModelSpec,
    handle: Handle<Gltf>,
}
#[derive(Resource, Default)]
pub struct ArtLibrary {
    pending: Vec<PendingModel>,
    pub models: BTreeMap<ModelKind, Vec<Prefab>>,
    pub color: Handle<Image>,
    pub detail: Handle<Image>,
    pub ready: bool,
    pub error: Option<String>,
}
pub fn start(mut commands: Commands, server: Res<AssetServer>) {
    let pending = manifest()
        .models
        .into_iter()
        .map(|spec| PendingModel {
            handle: server.load(spec.path.clone()),
            spec,
        })
        .collect();
    let color = server.load("art/polyhaven/ground-color.png");
    let detail = server
        .load_builder()
        .with_settings(|s: &mut ImageLoaderSettings| s.is_srgb = false)
        .load("art/polyhaven/ground-detail.png");
    commands.insert_resource(ArtLibrary {
        pending,
        color,
        detail,
        ..default()
    });
}

#[allow(clippy::too_many_arguments)] // Bevy asset preparation system.
pub fn poll(
    mut art: ResMut<ArtLibrary>,
    server: Res<AssetServer>,
    gltfs: Res<Assets<Gltf>>,
    nodes: Res<Assets<GltfNode>>,
    gltf_meshes: Res<Assets<GltfMesh>>,
    source_materials: Res<Assets<GltfMaterial>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
    mut ui: ResMut<UiState>,
) {
    if art.ready || art.error.is_some() {
        return;
    }
    let ids: Vec<_> = art
        .pending
        .iter()
        .map(|m| m.handle.id().untyped())
        .chain([art.color.id().untyped(), art.detail.id().untyped()])
        .collect();
    for id in ids {
        if let Some((state, _, dependencies)) = server.get_load_states(id) {
            let failed = match (state, dependencies) {
                (LoadState::Failed(e), _) | (_, RecursiveDependencyLoadState::Failed(e)) => Some(e),
                _ => None,
            };
            if let Some(e) = failed {
                let error = format!(
                    "Art asset loading failed: {e}. Restore the asset files; no placeholder models are used."
                );
                ui.error = Some(error.clone());
                art.error = Some(error);
                return;
            }
        }
        if !server.is_loaded_with_dependencies(id) {
            return;
        }
    }
    let result = (|| -> Result<BTreeMap<ModelKind, Vec<Prefab>>> {
        let mut prefabs = BTreeMap::<ModelKind, Vec<Prefab>>::new();
        for pending in &art.pending {
            let gltf = gltfs.get(&pending.handle).context("Loaded GLB missing")?;
            let prefab = prepare_model(
                gltf,
                &nodes,
                &gltf_meshes,
                &source_materials,
                &mut meshes,
                &mut materials,
                pending.spec.kind,
            )?;
            ensure!(
                !prefab.parts.is_empty(),
                "GLB has no renderable mesh: {}",
                pending.spec.path
            );
            prefabs.entry(pending.spec.kind).or_default().push(prefab);
        }
        for handle in [&art.color, &art.detail] {
            let mut image = images.get_mut(handle).context("Loaded PBR array missing")?;
            prepare_array(&mut image)?;
        }
        Ok(prefabs)
    })();
    match result {
        Ok(models) => {
            let count: usize = models.values().map(Vec::len).sum();
            art.models = models;
            art.ready = true;
            info!("ART: {count} imported GLB prefabs and 2 mipmapped Poly Haven arrays ready");
        }
        Err(e) => {
            let error = format!("Art preparation failed: {e:#}");
            ui.error = Some(error.clone());
            art.error = Some(error);
        }
    }
}

fn prepare_model(
    gltf: &Gltf,
    nodes: &Assets<GltfNode>,
    source_meshes: &Assets<GltfMesh>,
    source_materials: &Assets<GltfMaterial>,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    kind: ModelKind,
) -> Result<Prefab> {
    let children: BTreeSet<_> = gltf
        .nodes
        .iter()
        .filter_map(|h| nodes.get(h))
        .flat_map(|n| n.children.iter().map(|h| h.id()))
        .collect();
    let mut pending: Vec<_> = gltf
        .nodes
        .iter()
        .filter(|h| !children.contains(&h.id()))
        .map(|h| (h.clone(), Transform::IDENTITY))
        .collect();
    let mut parts = Vec::new();
    let mut low = Vec3::splat(f32::INFINITY);
    let mut high = Vec3::splat(f32::NEG_INFINITY);
    let mut material_cache =
        BTreeMap::<bevy::asset::AssetId<GltfMaterial>, Handle<StandardMaterial>>::new();
    while let Some((handle, parent)) = pending.pop() {
        let node = nodes.get(&handle).context("GLB node missing")?;
        ensure!(
            node.skin.is_none(),
            "Skinned models require an animation pipeline"
        );
        let transform = parent.mul_transform(node.transform);
        pending.extend(node.children.iter().map(|h| (h.clone(), transform)));
        if let Some(handle) = &node.mesh {
            let source = source_meshes.get(handle).context("GLB mesh missing")?;
            for primitive in &source.primitives {
                let mut mesh = meshes
                    .get(&primitive.mesh)
                    .context("GLB primitive missing")?
                    .clone();
                mesh.try_transform_by(transform)?;
                let Some(VertexAttributeValues::Float32x3(positions)) =
                    mesh.attribute(Mesh::ATTRIBUTE_POSITION)
                else {
                    bail!("GLB has no 3D positions");
                };
                for p in positions {
                    low = low.min(Vec3::from_array(*p));
                    high = high.max(Vec3::from_array(*p));
                }
                let source = primitive
                    .material
                    .as_ref()
                    .context("GLB material missing")?;
                let material = if let Some(handle) = material_cache.get(&source.id()) {
                    handle.clone()
                } else {
                    let m = source_materials
                        .get(source)
                        .context("GLB material unavailable")?;
                    let mut base_color = m.base_color;
                    if kind != ModelKind::Building {
                        // Grade the bright source palette to the GIS landscape.
                        // The kit's grass-capped rocks use a muted stone palette.
                        let c = base_color.to_linear();
                        base_color = if kind == ModelKind::Rock {
                            let tone = c.red * 0.35 + c.green * 0.5 + c.blue * 0.15;
                            Color::linear_rgba(
                                0.12 + tone * 0.10,
                                0.11 + tone * 0.095,
                                0.10 + tone * 0.08,
                                c.alpha,
                            )
                        } else {
                            Color::linear_rgba(
                                c.red * 0.30,
                                c.green * 0.22,
                                c.blue * 0.055,
                                c.alpha,
                            )
                        };
                    }
                    let handle = materials.add(StandardMaterial {
                        base_color,
                        base_color_channel: m.base_color_channel.clone(),
                        base_color_texture: m.base_color_texture.clone(),
                        uv_transform: m.uv_transform,
                        metallic: 0.,
                        perceptual_roughness: m.perceptual_roughness.max(0.8),
                        normal_map_texture: m.normal_map_texture.clone(),
                        normal_map_channel: m.normal_map_channel.clone(),
                        occlusion_texture: m.occlusion_texture.clone(),
                        occlusion_channel: m.occlusion_channel.clone(),
                        alpha_mode: m.alpha_mode,
                        double_sided: m.double_sided,
                        cull_mode: m.cull_mode,
                        ..default()
                    });
                    material_cache.insert(source.id(), handle.clone());
                    handle
                };
                parts.push((mesh, material));
            }
        }
    }
    ensure!(low.is_finite() && high.is_finite(), "GLB has empty bounds");
    let extent = high - low;
    let size = if matches!(kind, ModelKind::Tree | ModelKind::Conifer) {
        extent.y
    } else {
        extent.x.max(extent.z)
    };
    ensure!(size > 1e-5, "Degenerate GLB bounds");
    let origin = Vec3::new((low.x + high.x) * 0.5, low.y, (low.z + high.z) * 0.5);
    let normalize = Transform::from_translation(-origin / size).with_scale(Vec3::splat(1. / size));
    Ok(Prefab {
        parts: parts
            .into_iter()
            .map(|(mut mesh, material)| {
                mesh.transform_by(normalize);
                ModelPart {
                    mesh: meshes.add(mesh),
                    material,
                }
            })
            .collect(),
    })
}

fn prepare_array(image: &mut Image) -> Result<()> {
    ensure!(
        image.width() == 1024 && image.height() == 4096,
        "Invalid terrain array dimensions"
    );
    ensure!(
        matches!(
            image.texture_descriptor.format,
            TextureFormat::Rgba8Unorm | TextureFormat::Rgba8UnormSrgb
        ),
        "Terrain arrays must be RGBA8"
    );
    image.reinterpret_stacked_2d_as_array(4)?;
    let data = image
        .data
        .as_ref()
        .context("Terrain array has no CPU pixels")?;
    image.data = Some(mip_chain(data, 1024, 4));
    image.texture_descriptor.mip_level_count = 11;
    image.texture_view_descriptor = Some(TextureViewDescriptor {
        dimension: Some(TextureViewDimension::D2Array),
        ..default()
    });
    image.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
        address_mode_u: ImageAddressMode::Repeat,
        address_mode_v: ImageAddressMode::Repeat,
        mag_filter: ImageFilterMode::Linear,
        min_filter: ImageFilterMode::Linear,
        mipmap_filter: ImageFilterMode::Linear,
        anisotropy_clamp: 4,
        ..default()
    });
    Ok(())
}
// Image's default data order is layer-major: every mip of layer 0, then layer 1.
fn mip_chain(data: &[u8], size: usize, layers: usize) -> Vec<u8> {
    let mut output = Vec::with_capacity(data.len() * 4 / 3 + layers * 4);
    for layer in 0..layers {
        let mut pixels = data[layer * size * size * 4..(layer + 1) * size * size * 4].to_vec();
        let mut width = size;
        loop {
            output.extend_from_slice(&pixels);
            if width == 1 {
                break;
            }
            let next = width / 2;
            let mut smaller = vec![0; next * next * 4];
            for y in 0..next {
                for x in 0..next {
                    for c in 0..4 {
                        let sum: u32 = [0, 1]
                            .into_iter()
                            .flat_map(|dy| [0, 1].map(move |dx| (dy, dx)))
                            .map(|(dy, dx)| {
                                u32::from(pixels[((y * 2 + dy) * width + x * 2 + dx) * 4 + c])
                            })
                            .sum();
                        smaller[(y * next + x) * 4 + c] = ((sum + 2) / 4) as u8;
                    }
                }
            }
            pixels = smaller;
            width = next;
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn committed_assets_are_hydrated_and_match_the_import_manifest() {
        validate_root(&Path::new(env!("CARGO_MANIFEST_DIR")).join("assets")).unwrap();
    }
    #[test]
    fn mipmaps_keep_array_layers_separate_and_include_the_last_level() {
        let data: Vec<_> = [10_u8, 70, 130, 250]
            .into_iter()
            .flat_map(|v| [v; 64])
            .collect();
        let mips = mip_chain(&data, 4, 4);
        assert_eq!(mips.len(), 4 * (64 + 16 + 4));
        for (layer, value) in [10, 70, 130, 250].into_iter().enumerate() {
            assert!(
                mips[layer * 84..(layer + 1) * 84]
                    .iter()
                    .all(|v| *v == value)
            );
        }
    }
}
