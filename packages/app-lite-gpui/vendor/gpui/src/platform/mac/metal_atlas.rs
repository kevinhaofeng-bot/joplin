use crate::{
    AtlasKey, AtlasTextureId, AtlasTextureKind, AtlasTile, Bounds, DevicePixels, PlatformAtlas,
    Point, Size, platform::AtlasTextureList,
};
use anyhow::{Context as _, Result};
use collections::FxHashMap;
use derive_more::{Deref, DerefMut};
use etagere::BucketedAtlasAllocator;
use metal::Device;
use parking_lot::Mutex;
use std::borrow::Cow;

pub(crate) struct MetalAtlas(Mutex<MetalAtlasState>);

impl MetalAtlas {
    pub(crate) fn new(device: Device) -> Self {
        MetalAtlas(Mutex::new(MetalAtlasState {
            device: AssertSend(device),
            monochrome_textures: Default::default(),
            polychrome_textures: Default::default(),
            tiles_by_key: Default::default(),
        }))
    }

    pub(crate) fn metal_texture(&self, id: AtlasTextureId) -> metal::Texture {
        self.0.lock().texture(id).metal_texture.clone()
    }
}

struct MetalAtlasState {
    device: AssertSend<Device>,
    monochrome_textures: AtlasTextureList<MetalAtlasTexture>,
    polychrome_textures: AtlasTextureList<MetalAtlasTexture>,
    tiles_by_key: FxHashMap<AtlasKey, AtlasTile>,
}

impl PlatformAtlas for MetalAtlas {
    fn get_or_insert_with<'a>(
        &self,
        key: &AtlasKey,
        build: &mut dyn FnMut() -> Result<Option<(Size<DevicePixels>, Cow<'a, [u8]>)>>,
    ) -> Result<Option<AtlasTile>> {
        let mut lock = self.0.lock();
        if let Some(tile) = lock.tiles_by_key.get(key) {
            Ok(Some(tile.clone()))
        } else {
            let Some((size, bytes)) = build()? else {
                return Ok(None);
            };
            let tile = lock
                .allocate(size, key.texture_kind())
                .context("failed to allocate")?;
            let texture = lock.texture(tile.texture_id);
            texture.upload(tile.bounds, &bytes);
            lock.tiles_by_key.insert(key.clone(), tile.clone());
            Ok(Some(tile))
        }
    }

    fn remove(&self, key: &AtlasKey) {
        let mut lock = self.0.lock();
        // Remove the key even when another tile keeps the texture alive. This
        // also makes repeated drops harmless and allows a later upload to rebuild.
        let Some(tile) = lock.tiles_by_key.remove(key) else {
            return;
        };
        let id = tile.texture_id;

        let textures = match id.kind {
            AtlasTextureKind::Monochrome => &mut lock.monochrome_textures,
            AtlasTextureKind::Polychrome => &mut lock.polychrome_textures,
        };

        let Some(texture_slot) = textures
            .textures
            .iter_mut()
            .find(|texture| texture.as_ref().is_some_and(|v| v.id == id))
        else {
            return;
        };

        if let Some(mut texture) = texture_slot.take() {
            texture.allocator.deallocate(tile.tile_id.into());
            texture.decrement_ref_count();

            if texture.is_unreferenced() {
                textures.free_list.push(id.index as usize);
            } else {
                *texture_slot = Some(texture);
            }
        }
    }
}

impl MetalAtlasState {
    fn allocate(
        &mut self,
        size: Size<DevicePixels>,
        texture_kind: AtlasTextureKind,
    ) -> Option<AtlasTile> {
        {
            let textures = match texture_kind {
                AtlasTextureKind::Monochrome => &mut self.monochrome_textures,
                AtlasTextureKind::Polychrome => &mut self.polychrome_textures,
            };

            if let Some(tile) = textures
                .iter_mut()
                .rev()
                .find_map(|texture| texture.allocate(size))
            {
                return Some(tile);
            }
        }

        let texture = self.push_texture(size, texture_kind);
        texture.allocate(size)
    }

    fn push_texture(
        &mut self,
        min_size: Size<DevicePixels>,
        kind: AtlasTextureKind,
    ) -> &mut MetalAtlasTexture {
        const DEFAULT_ATLAS_SIZE: Size<DevicePixels> = Size {
            width: DevicePixels(1024),
            height: DevicePixels(1024),
        };
        // Max texture size on all modern Apple GPUs. Anything bigger than that crashes in validateWithDevice.
        const MAX_ATLAS_SIZE: Size<DevicePixels> = Size {
            width: DevicePixels(16384),
            height: DevicePixels(16384),
        };
        let size = min_size.min(&MAX_ATLAS_SIZE).max(&DEFAULT_ATLAS_SIZE);
        let texture_descriptor = metal::TextureDescriptor::new();
        texture_descriptor.set_width(size.width.into());
        texture_descriptor.set_height(size.height.into());
        let pixel_format;
        let usage;
        match kind {
            AtlasTextureKind::Monochrome => {
                pixel_format = metal::MTLPixelFormat::A8Unorm;
                usage = metal::MTLTextureUsage::ShaderRead;
            }
            AtlasTextureKind::Polychrome => {
                pixel_format = metal::MTLPixelFormat::BGRA8Unorm;
                usage = metal::MTLTextureUsage::ShaderRead;
            }
        }
        texture_descriptor.set_pixel_format(pixel_format);
        texture_descriptor.set_usage(usage);
        let metal_texture = self.device.new_texture(&texture_descriptor);

        let texture_list = match kind {
            AtlasTextureKind::Monochrome => &mut self.monochrome_textures,
            AtlasTextureKind::Polychrome => &mut self.polychrome_textures,
        };

        let index = texture_list.free_list.pop();

        let atlas_texture = MetalAtlasTexture {
            id: AtlasTextureId {
                index: index.unwrap_or(texture_list.textures.len()) as u32,
                kind,
            },
            allocator: etagere::BucketedAtlasAllocator::new(size.into()),
            metal_texture: AssertSend(metal_texture),
            live_atlas_keys: 0,
        };

        if let Some(ix) = index {
            texture_list.textures[ix] = Some(atlas_texture);
            texture_list.textures.get_mut(ix)
        } else {
            texture_list.textures.push(Some(atlas_texture));
            texture_list.textures.last_mut()
        }
        .unwrap()
        .as_mut()
        .unwrap()
    }

    fn texture(&self, id: AtlasTextureId) -> &MetalAtlasTexture {
        let textures = match id.kind {
            crate::AtlasTextureKind::Monochrome => &self.monochrome_textures,
            crate::AtlasTextureKind::Polychrome => &self.polychrome_textures,
        };
        textures[id.index as usize].as_ref().unwrap()
    }
}

struct MetalAtlasTexture {
    id: AtlasTextureId,
    allocator: BucketedAtlasAllocator,
    metal_texture: AssertSend<metal::Texture>,
    live_atlas_keys: u32,
}

impl MetalAtlasTexture {
    fn allocate(&mut self, size: Size<DevicePixels>) -> Option<AtlasTile> {
        let allocation = self.allocator.allocate(size.into())?;
        let tile = AtlasTile {
            texture_id: self.id,
            tile_id: allocation.id.into(),
            bounds: Bounds {
                origin: allocation.rectangle.min.into(),
                size,
            },
            padding: 0,
        };
        self.live_atlas_keys += 1;
        Some(tile)
    }

    fn upload(&self, bounds: Bounds<DevicePixels>, bytes: &[u8]) {
        let region = metal::MTLRegion::new_2d(
            bounds.origin.x.into(),
            bounds.origin.y.into(),
            bounds.size.width.into(),
            bounds.size.height.into(),
        );
        self.metal_texture.replace_region(
            region,
            0,
            bytes.as_ptr() as *const _,
            bounds.size.width.to_bytes(self.bytes_per_pixel()) as u64,
        );
    }

    fn bytes_per_pixel(&self) -> u8 {
        use metal::MTLPixelFormat::*;
        match self.metal_texture.pixel_format() {
            A8Unorm | R8Unorm => 1,
            RGBA8Unorm | BGRA8Unorm => 4,
            _ => unimplemented!(),
        }
    }

    fn decrement_ref_count(&mut self) {
        self.live_atlas_keys -= 1;
    }

    fn is_unreferenced(&mut self) -> bool {
        self.live_atlas_keys == 0
    }
}

impl From<Size<DevicePixels>> for etagere::Size {
    fn from(size: Size<DevicePixels>) -> Self {
        etagere::Size::new(size.width.into(), size.height.into())
    }
}

impl From<etagere::Point> for Point<DevicePixels> {
    fn from(value: etagere::Point) -> Self {
        Point {
            x: DevicePixels::from(value.x),
            y: DevicePixels::from(value.y),
        }
    }
}

impl From<etagere::Size> for Size<DevicePixels> {
    fn from(size: etagere::Size) -> Self {
        Size {
            width: DevicePixels::from(size.width),
            height: DevicePixels::from(size.height),
        }
    }
}

impl From<etagere::Rectangle> for Bounds<DevicePixels> {
    fn from(rectangle: etagere::Rectangle) -> Self {
        Bounds {
            origin: rectangle.min.into(),
            size: rectangle.size().into(),
        }
    }
}

#[derive(Deref, DerefMut)]
struct AssertSend<T>(T);

unsafe impl<T> Send for AssertSend<T> {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ImageId, RenderImageParams};

    fn image_key(id: usize) -> AtlasKey {
        AtlasKey::Image(RenderImageParams {
            image_id: ImageId(id),
            frame_index: 0,
        })
    }

    fn atlas() -> MetalAtlas {
        MetalAtlas::new(Device::system_default().expect("real Metal device required"))
    }

    fn insert(atlas: &MetalAtlas, key: &AtlasKey, side: i32, bgra: [u8; 4]) -> AtlasTile {
        let bytes = bgra.repeat((side * side) as usize);
        atlas
            .get_or_insert_with(key, &mut || {
                Ok(Some((
                    Size {
                        width: DevicePixels(side),
                        height: DevicePixels(side),
                    },
                    Cow::Borrowed(&bytes),
                )))
            })
            .unwrap()
            .unwrap()
    }

    fn pixel(atlas: &MetalAtlas, tile: &AtlasTile) -> [u8; 4] {
        let texture = atlas.metal_texture(tile.texture_id);
        let mut bytes = [0u8; 4];
        texture.get_bytes(
            bytes.as_mut_ptr().cast(),
            4,
            metal::MTLRegion::new_2d(
                tile.bounds.origin.x.into(),
                tile.bounds.origin.y.into(),
                1,
                1,
            ),
            0,
        );
        bytes
    }

    #[test]
    fn removed_shared_image_is_rebuilt_with_new_pixels() {
        let atlas = atlas();
        let a = image_key(1);
        let b = image_key(2);
        let first = insert(&atlas, &a, 32, [11, 22, 33, 255]);
        let sibling = insert(&atlas, &b, 32, [44, 55, 66, 255]);
        assert_eq!(first.texture_id, sibling.texture_id);
        assert_eq!(pixel(&atlas, &first), [11, 22, 33, 255]);
        atlas.remove(&a);
        let replacement = insert(&atlas, &a, 32, [77, 88, 99, 255]);
        assert_eq!(pixel(&atlas, &replacement), [77, 88, 99, 255]);
        assert_eq!(pixel(&atlas, &sibling), [44, 55, 66, 255]);
    }

    #[test]
    fn removing_same_image_twice_keeps_sibling_texture_alive() {
        let atlas = atlas();
        let a = image_key(1);
        let first = insert(&atlas, &a, 32, [11, 22, 33, 255]);
        let sibling = insert(&atlas, &image_key(2), 32, [44, 55, 66, 255]);
        assert_eq!(first.texture_id, sibling.texture_id);
        atlas.remove(&a);
        atlas.remove(&a);
        assert_eq!(pixel(&atlas, &sibling), [44, 55, 66, 255]);
    }

    #[test]
    fn removed_image_space_is_reused_while_sibling_remains_live() {
        let atlas = atlas();
        let anchor = insert(&atlas, &image_key(1), 4, [11, 22, 33, 255]);
        for id in 2..14 {
            let key = image_key(id);
            let temporary = insert(&atlas, &key, 512, [44, 55, 66, 255]);
            assert_eq!(
                temporary.texture_id, anchor.texture_id,
                "released space should be reused instead of growing the atlas at image {id}"
            );
            assert_eq!(pixel(&atlas, &temporary), [44, 55, 66, 255]);
            atlas.remove(&key);
            assert_eq!(pixel(&atlas, &anchor), [11, 22, 33, 255]);
        }
    }
}
