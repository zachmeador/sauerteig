struct LightCamera { view_proj: mat4x4<f32> };
@group(0) @binding(0) var<uniform> light: LightCamera;
@vertex fn vs_main(@location(0) position: vec3<f32>, @location(6) instance: vec4<f32>) -> @builtin(position) vec4<f32> {
    let c = cos(instance.w); let s = sin(instance.w);
    let p = vec3<f32>(c*position.x+s*position.z, position.y, -s*position.x+c*position.z) + instance.xyz;
    return light.view_proj * vec4<f32>(p, 1.0);
}
