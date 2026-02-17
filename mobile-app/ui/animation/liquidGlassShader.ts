/**
 * SkSL shader for SDF-based liquid glass morph animation.
 *
 * Renders up to 5 tab indicator circles that merge with smooth-minimum (smin)
 * blending, producing the liquid-glass "droplet merge" effect from the Shopify
 * react-native-skia examples.
 *
 * Uniforms are driven from Reanimated shared values so the morph is 60 fps.
 */
export const LIQUID_MORPH_SKSL = `
uniform float2 resolution;
uniform float progress;
uniform float k;
uniform float cy;

uniform float t0x;
uniform float t0r;
uniform float t1x;
uniform float t1r;
uniform float t2x;
uniform float t2r;
uniform float t3x;
uniform float t3r;
uniform float t4x;
uniform float t4r;

float sdCircle(float2 p, float2 c, float r) {
    return length(p - c) - r;
}

float smin(float a, float b, float smoothK) {
    float h = clamp(0.5 + 0.5 * (a - b) / smoothK, 0.0, 1.0);
    return mix(a, b, h) - smoothK * h * (1.0 - h);
}

half4 main(float2 xy) {
    float d = 10000.0;

    // Accumulate SDF for each active circle
    if (t0r > 0.5) {
        float dd = sdCircle(xy, float2(t0x, cy), t0r);
        d = (d > 9999.0) ? dd : smin(d, dd, k);
    }
    if (t1r > 0.5) {
        float dd = sdCircle(xy, float2(t1x, cy), t1r);
        d = (d > 9999.0) ? dd : smin(d, dd, k);
    }
    if (t2r > 0.5) {
        float dd = sdCircle(xy, float2(t2x, cy), t2r);
        d = (d > 9999.0) ? dd : smin(d, dd, k);
    }
    if (t3r > 0.5) {
        float dd = sdCircle(xy, float2(t3x, cy), t3r);
        d = (d > 9999.0) ? dd : smin(d, dd, k);
    }
    if (t4r > 0.5) {
        float dd = sdCircle(xy, float2(t4x, cy), t4r);
        d = (d > 9999.0) ? dd : smin(d, dd, k);
    }

    // Shape alpha — smooth falloff at edges
    float alpha = 1.0 - smoothstep(-2.0, 1.5, d);
    if (alpha < 0.005) return half4(0.0, 0.0, 0.0, 0.0);

    // Edge highlight ring
    float edge = 1.0 - smoothstep(-5.0, -0.5, d);

    // Glass gradient: primary → accent across X
    float t = xy.x / max(resolution.x, 1.0);
    half3 primary  = half3(0.831, 0.957, 0.612);
    half3 accent   = half3(0.545, 0.361, 0.965);
    half3 col = mix(primary, accent, t * 0.45 + edge * 0.55);

    // Inner glow at center of each blob
    float inner = 1.0 - smoothstep(-8.0, -2.0, d);
    col += half3(0.15) * inner;

    // Fade out as morph completes
    float morphFade = 1.0 - progress * progress;
    float finalAlpha = alpha * 0.28 * morphFade;

    return half4(col * finalAlpha, finalAlpha);
}
`;

/**
 * Simplified fallback shader for devices where the full SDF shader fails.
 * Just draws soft circles without smin blending.
 */
export const LIQUID_MORPH_FALLBACK_SKSL = `
uniform float2 resolution;
uniform float progress;
uniform float cy;

uniform float t0x; uniform float t0r;
uniform float t1x; uniform float t1r;
uniform float t2x; uniform float t2r;
uniform float t3x; uniform float t3r;
uniform float t4x; uniform float t4r;

half4 main(float2 xy) {
    float alpha = 0.0;

    if (t0r > 0.5) alpha += max(0.0, 1.0 - length(xy - float2(t0x, cy)) / t0r);
    if (t1r > 0.5) alpha += max(0.0, 1.0 - length(xy - float2(t1x, cy)) / t1r);
    if (t2r > 0.5) alpha += max(0.0, 1.0 - length(xy - float2(t2x, cy)) / t2r);
    if (t3r > 0.5) alpha += max(0.0, 1.0 - length(xy - float2(t3x, cy)) / t3r);
    if (t4r > 0.5) alpha += max(0.0, 1.0 - length(xy - float2(t4x, cy)) / t4r);

    alpha = min(alpha, 1.0);
    if (alpha < 0.01) return half4(0.0, 0.0, 0.0, 0.0);

    float morphFade = 1.0 - progress * progress;
    float a = alpha * 0.25 * morphFade;

    half3 col = mix(half3(0.831, 0.957, 0.612), half3(0.545, 0.361, 0.965), xy.x / max(resolution.x, 1.0));
    return half4(col * a, a);
}
`;
