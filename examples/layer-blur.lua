-- Enable blur behind selected rectangular layer-shell surfaces.
-- Use the namespace reported/configured by your bar or notification center.
tomoe.settings {
  blur = {
    enabled = true,
    passes = 3,
    offset = 1.0,
    -- Include source changes just outside the visible surface in the blur.
    anti_artifact_margin = 96,
    layer_namespaces = { "waybar", "swaync-control-center" },
  },
}
