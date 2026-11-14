struct LightCamera { view_proj: mat4x4<f32> };
fn rotate_y(p: vec3<f32>, yaw: f32) -> vec3<f32> {
    let c = cos(yaw); let s = sin(yaw);
    return vec3<f32>(c*p.x + s*p.z, p.y, -s*p.x + c*p.z);
}
struct Camera {
    view_proj: mat4x4<f32>,
    light_view_proj: mat4x4<f32>,
    eye_time: vec4<f32>,
    sun: vec4<f32>,
    fog: vec4<f32>,
};
// One light: position and range; color times intensity times fade, and the
// spot cone's scale; spot direction and cone offset; and flags (bit 0 spot,
// bits 1-2 shadow mode, 3.. overlay slots) with six 16-bit shadow tile
// references. A spot light keeps its tile's tangent in packed.z.
struct Light {
    position_range: vec4<f32>,
    color_scale: vec4<f32>,
    direction_offset: vec4<f32>,
    packed: vec4<u32>,
};
struct LightParams {
    // Clusters across, down and in depth, then the light count.
    grid: vec4<u32>,
    // Near plane, end of the first slice, slice growth, and its 1 / ln.
    depth: vec4<f32>,
    // Scene target size, then the projection's x and y scales.
    screen: vec4<f32>,
    // Far plane.
    range: vec4<f32>,
    // 1 = read cluster masks, 0 = every light; two bits of tile size per page;
    // the overlay page; lights per fragment.
    mode: vec4<u32>,
};
// Camera and lighting bindings follow the reusable types.
@group(0) @binding(0) var<uniform> camera: Camera;
@group(1) @binding(0) var shadow_map: texture_depth_2d;
@group(1) @binding(1) var shadow_sampler: sampler_comparison;
@group(1) @binding(2) var atlas: texture_depth_2d_array;
@group(1) @binding(5) var<uniform> light_params: LightParams;
@group(1) @binding(3) var<storage, read> lights: array<Light>;
@group(1) @binding(4) var<storage, read> cluster_masks: array<u32>;
fn light_at(i: u32) -> Light { return lights[i]; }
fn light_sphere(i: u32) -> vec4<f32> { return lights[i].position_range; }
fn mask_word(cluster: u32, word: u32) -> u32 { return cluster_masks[cluster * MASK_WORDS + word]; }

// Depth changes across a sloping receiver. Comparing every PCF tap against
// the center depth stripes the surface; project its tangent plane instead.
fn depth_gradient(matrix: mat4x4<f32>, clip: vec4<f32>, normal: vec3<f32>) -> vec2<f32> {
    let axis = select(vec3<f32>(0.0, 1.0, 0.0), vec3<f32>(1.0, 0.0, 0.0), abs(normal.y) > 0.9);
    let tangent = normalize(cross(normal, axis));
    let bitangent = cross(normal, tangent);
    let a = matrix * vec4<f32>(tangent, 0.0);
    let b = matrix * vec4<f32>(bitangent, 0.0);
    let ndc = clip.xyz / clip.w;
    let da = (a.xyz - ndc * a.w) / clip.w;
    let db = (b.xyz - ndc * b.w) / clip.w;
    let ua = da.xy * vec2<f32>(0.5, -0.5);
    let ub = db.xy * vec2<f32>(0.5, -0.5);
    let det = ua.x * ub.y - ua.y * ub.x;
    if (abs(det) < 0.00000001) { return vec2<f32>(0.0); }
    return vec2<f32>(da.z * ub.y - db.z * ua.y, ua.x * db.z - ub.x * da.z) / det;
}

// ATLAS_SIZE, SHADOW_NEAR, TILE_GUARD_SCALE, NO_TILE, OVERLAY_NONE, MASK_WORDS and
// MAX_LIGHTS are defined ahead of this module by the assembler.
// Receiver offset along the normal, and depth bias, in tile texels.
const NORMAL_OFFSET: f32 = 1.5;
const DEPTH_BIAS: f32 = 1.0;
const TRANSPARENT_LIGHTS: u32 = 8u;
// Irradiance below which a light is skipped, and above which its shadow takes
// the full four taps. The sun adds about 0.85.
const FAINT_LIGHT: f32 = 0.004;
const SHARP_LIGHT: f32 = 0.05;

struct Surface {
    position: vec3<f32>,
    normal: vec3<f32>,
    geometric: vec3<f32>,
    view: vec3<f32>,
    specular_power: f32,
    // Water and glass show only highlights: lights whose highlight misses the
    // eye are skipped.
    specular_only: bool,
};
struct LightTerm {
    diffuse: vec3<f32>,
    specular: vec3<f32>,
    used: bool,
};
struct LightSum {
    diffuse: vec3<f32>,
    specular: vec3<f32>,
};

fn fog_factor(position: vec3<f32>) -> f32 {
    let distance = length(camera.eye_time.xyz - position);
    return clamp(1.0 - exp(-distance * distance * camera.fog.w), 0.0, 0.85);
}
fn apply_fog(color: vec3<f32>, position: vec3<f32>) -> vec3<f32> {
    return mix(color, camera.fog.rgb, fog_factor(position));
}

fn cluster_index(frag: vec4<f32>) -> u32 {
    let depth = 1.0 / frag.w;
    let gx = light_params.grid.x;
    let gy = light_params.grid.y;
    let gz = light_params.grid.z;
    let x = min(u32(frag.x * f32(gx) / light_params.screen.x), gx - 1u);
    let y = min(u32(frag.y * f32(gy) / light_params.screen.y), gy - 1u);
    var slice = 0u;
    if (depth >= light_params.depth.y) {
        slice = min(u32(log(depth / light_params.depth.y) * light_params.depth.w) + 1u, gz - 1u);
    }
    return (slice * gy + y) * gx + x;
}

// Depth of a point `distance` along a tile's axis, as the tile stores it.
fn tile_depth(distance: f32, far: f32) -> f32 {
    let d = max(distance, SHADOW_NEAR * 1.01);
    return far * (d - SHADOW_NEAR) / ((far - SHADOW_NEAR) * d);
}

// Fraction of a light reaching a point through one shadow tile. `to_point` is
// from the light, in the tile's frame (right, up, forward). Each tap compares
// against the depth the receiver's own tangent plane has at that tap, so a
// sloped surface does not shadow itself.
fn tile_visibility(
    tile: u32, tan_half: f32,
    right: vec3<f32>, up: vec3<f32>, forward: vec3<f32>,
    far: f32, to_point: vec3<f32>, normal: vec3<f32>, taps: u32,
) -> f32 {
    let page = tile >> 8u;
    let slot = tile & 255u;
    let size_code = (light_params.mode.y >> (page * 2u)) & 3u;
    let size = 128.0 * f32(1u << size_code);
    let per_row = u32(ATLAS_SIZE / size);
    let origin = vec2<f32>(f32(slot % per_row), f32(slot / per_row)) * size;

    let near_depth = dot(to_point, forward);
    let texel = 2.0 * tan_half * max(near_depth, SHADOW_NEAR) / size;
    let along = dot(normal, normalize(to_point));
    let sine = sqrt(max(1.0 - along * along, 0.0));
    let moved = to_point + normal * (texel * NORMAL_OFFSET * (0.25 + sine));
    let x = dot(moved, right);
    let y = dot(moved, up);
    let d = dot(moved, forward);
    if (d <= SHADOW_NEAR * 1.5) { return 1.0; }
    let a = x / d;
    let b = y / d;
    let uv = vec2<f32>(0.5 + 0.5 * a / tan_half, 0.5 - 0.5 * b / tan_half);
    if (any(uv < vec2<f32>(0.0)) || any(uv > vec2<f32>(1.0))) { return 1.0; }

    let np = vec3<f32>(dot(normal, right), dot(normal, up), dot(normal, forward));
    let den = min(np.x * a + np.y * b + np.z, -0.15);
    let slope = vec2<f32>(np.x, np.y) * (-d / den);
    let limit = texel * 3.0;
    let bias = texel * DEPTH_BIAS;
    let texel_tan = tan_half / size;
    if (taps == 1u) {
        let tap = (origin + uv * size) / ATLAS_SIZE;
        return textureSampleCompareLevel(atlas, shadow_sampler, tap, i32(page), tile_depth(d - bias, far));
    }
    var sum = 0.0;
    for (var i = 0u; i < 4u; i += 1u) {
        let sx = select(-0.5, 0.5, (i & 1u) != 0u);
        let sy = select(-0.5, 0.5, (i & 2u) != 0u);
        let reference = d + clamp((slope.x * sx - slope.y * sy) * 2.0 * texel_tan, -limit, limit) - bias;
        let tap = (origin + uv * size + vec2<f32>(sx, sy)) / ATLAS_SIZE;
        sum += textureSampleCompareLevel(atlas, shadow_sampler, tap, i32(page), tile_depth(reference, far));
    }
    return sum * 0.25;
}

// Fraction of a light's illumination that reaches the surface.
fn light_visibility(l: Light, s: Surface, taps: u32) -> f32 {
    let spot = (l.packed.x & 1u) != 0u;
    let to_point = s.position - l.position_range.xyz;
    var face = 0u;
    var forward = vec3<f32>(0.0);
    var up = vec3<f32>(0.0, 1.0, 0.0);
    var tan_half = TILE_GUARD_SCALE;
    if (spot) {
        forward = l.direction_offset.xyz;
        if (abs(forward.y) > 0.99) { up = vec3<f32>(0.0, 0.0, 1.0); }
        tan_half = bitcast<f32>(l.packed.z);
    } else {
        let a = abs(to_point);
        var axis = 0u;
        if (a.y >= a.x && a.y >= a.z) { axis = 1u; } else if (a.z >= a.x && a.z >= a.y) { axis = 2u; }
        let negative = to_point[axis] < 0.0;
        face = axis * 2u + select(0u, 1u, negative);
        forward[axis] = select(1.0, -1.0, negative);
        if (axis == 1u) { up = vec3<f32>(0.0, 0.0, 1.0); }
    }
    let pair = l.packed[1u + face / 2u];
    let tile = (pair >> ((face & 1u) * 16u)) & 0xFFFFu;
    if (tile == NO_TILE) { return 1.0; }
    let right = normalize(cross(forward, up));
    let true_up = cross(right, forward);
    let far = l.position_range.w;
    var visibility = tile_visibility(tile, tan_half, right, true_up, forward, far, to_point, s.geometric, taps);
    if (((l.packed.x >> 1u) & 3u) == 2u) {
        let slot = (l.packed.x >> (3u + 4u * face)) & 15u;
        if (slot != OVERLAY_NONE) {
            let overlay = (light_params.mode.z << 8u) | slot;
            visibility = min(visibility, tile_visibility(overlay, tan_half, right, true_up, forward, far, to_point, s.geometric, taps));
        }
    }
    return visibility;
}

fn light_term(i: u32, s: Surface) -> LightTerm {
    var t: LightTerm;
    t.diffuse = vec3<f32>(0.0);
    t.specular = vec3<f32>(0.0);
    t.used = false;
    let sphere = light_sphere(i);
    let to_light = sphere.xyz - s.position;
    let d2 = dot(to_light, to_light);
    let r2 = sphere.w * sphere.w;
    if (d2 >= r2) { return t; }
    let direction = to_light * inverseSqrt(max(d2, 0.0001));
    let n_dot_l = dot(s.normal, direction);
    if (n_dot_l <= 0.0) { return t; }
    let l = light_at(i);
    let window = 1.0 - d2 / r2;
    var attenuation = window * window / (1.0 + d2);
    if ((l.packed.x & 1u) != 0u) {
        let cone = clamp(dot(-direction, l.direction_offset.xyz) * l.color_scale.w + l.direction_offset.w, 0.0, 1.0);
        attenuation *= cone * cone;
    }
    // How much the light can add, ignoring its shadow: lights too faint to see
    // are skipped without counting toward the cap, and lights that add little
    // filter their shadows with one tap.
    var weight = attenuation * n_dot_l * max(l.color_scale.r, max(l.color_scale.g, l.color_scale.b));
    var lobe = 1.0;
    if (s.specular_only) {
        lobe = pow(max(dot(s.normal, normalize(direction + s.view)), 0.0), s.specular_power);
        weight *= lobe;
    }
    if (weight < FAINT_LIGHT) { return t; }
    t.used = true;
    var visibility = 1.0;
    if (((l.packed.x >> 1u) & 3u) != 0u) {
        visibility = light_visibility(l, s, select(1u, 4u, weight >= SHARP_LIGHT));
    }
    let radiance = l.color_scale.rgb * (attenuation * visibility);
    if (s.specular_only) {
        t.specular = radiance * (lobe * n_dot_l);
        return t;
    }
    t.diffuse = radiance * n_dot_l;
    let half_vector = normalize(direction + s.view);
    t.specular = radiance * (pow(max(dot(s.normal, half_vector), 0.0), s.specular_power) * n_dot_l);
    return t;
}

// The lights of a fragment's cluster, or every light in list mode, up to
// `cap` of those that reach the surface, most important first.
fn shade_local_lights(frag: vec4<f32>, s: Surface, cap: u32) -> LightSum {
    var sum: LightSum;
    sum.diffuse = vec3<f32>(0.0);
    sum.specular = vec3<f32>(0.0);
    var used = 0u;
    if (light_params.mode.x == 1u) {
        let cluster = cluster_index(frag);
        for (var w = 0u; w < MASK_WORDS; w += 1u) {
            var bits = mask_word(cluster, w);
            while (bits != 0u) {
                let bit = firstTrailingBit(bits);
                bits = bits & (bits - 1u);
                // Masks are not rewritten in frames without lights.
                if (w * 32u + bit >= light_params.grid.w) { return sum; }
                let term = light_term(w * 32u + bit, s);
                if (term.used) {
                    sum.diffuse += term.diffuse;
                    sum.specular += term.specular;
                    used += 1u;
                    if (used >= cap) { return sum; }
                }
            }
        }
    } else {
        let count = min(light_params.grid.w, MAX_LIGHTS);
        for (var i = 0u; i < count; i += 1u) {
            let term = light_term(i, s);
            if (term.used) {
                sum.diffuse += term.diffuse;
                sum.specular += term.specular;
                used += 1u;
                if (used >= cap) { return sum; }
            }
        }
    }
    return sum;
}
