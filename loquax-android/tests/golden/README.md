# Golden pictures

`map-florence-z16.png`: the map screen's frame as the app gets it through
the facade (1128), drawn by `tests/map.rs`,
`the_frame_is_the_shared_renderers_with_names_and_a_pin`, from
`../../../leviculum-map/fixtures/firenze.lmr` at z16, 480 x 320, density
1, light, one pin: loquax's picture, the map's names and the pin with its
label, all from loquax's shared renderer (`loquax::screenmap`). Compared
pixel for pixel; a picture that moved is left in the temp dir as
`loquax-android-golden/<name>` for a look. The names are set in Fira Sans
as loquax carries it (`loquax/fonts/`), so the picture does not depend on
the host's fonts.

The picture is a produced work of OpenStreetMap data: © OpenStreetMap
contributors, <https://www.openstreetmap.org/copyright>, under the Open
Database License (ODbL) 1.0; see `leviculum-map/fixtures/README.md`.
