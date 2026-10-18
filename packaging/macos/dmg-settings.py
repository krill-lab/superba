# dmgbuild settings of Superba's disk image (user bug 84), used by
# scripts/release/macos-dmg.sh; dmgbuild runs this file with `defines` (its -D values)
# set.
#
# The image made from it is only the layout: the Finder window, where the icons go,
# the background, the Applications link and the volume icon, without any of
# Superba's files. macos-dmg.sh copies the release files into it afterwards with
# hdiutil and ditto, so in the release workflow dmgbuild (third-party code) runs
# before any signing key exists. Finder keys icon positions by name, so they apply to
# files added later.

format = "UDRW"
filesystem = "HFS+"
files = []
symlinks = {"Applications": "/Applications"}

# .VolumeIcon.icns (the app icon, scripts/release/Superba.icns) for the mounted volume.
icon = defines["volume_icon"]
# dmg-background.png; dmgbuild finds dmg-background@2x.png beside it and joins the two
# into one multi-resolution .background.tiff (tiffutil -cathidpicheck).
background = defines["background"]

# The window: no toolbar, path bar, sidebar, tab bar or status bar, only the title bar
# ("Superba <version>"). Finder reads window_rect as the whole frame, title bar
# included, with its origin at the bottom left of the screen: 428 pt are the 400 pt of
# the background under the 28 pt title bar of macOS 14 and 15 (macOS 26's is 32 pt and
# covers the background's bottom 4 pt, which hold only the glow). Icon positions below
# count from the top left of the area under the title bar.
window_rect = ((240, 300), (640, 428))
default_view = "icon-view"
show_toolbar = False
show_pathbar = False
show_sidebar = False
show_tab_view = False
show_status_bar = False
show_icon_preview = False
include_icon_view_settings = True
include_list_view_settings = False
arrange_by = None
icon_size = 128
text_size = 14
label_pos = "bottom"

# Icon centres in window points; dmg-background.svg draws the arrow between the two
# and the hint below them, so move them together.
icon_locations = {
    "Superba.app": (170, 170),
    "Applications": (470, 170),
}
# Everything else in the image is hidden in Finder (macos-dmg.sh sets UF_HIDDEN on
# all release files but Superba.app). Should someone show hidden files, these sit
# below the window instead of over the background.
for column, name in enumerate(
    (
        "spb",
        "README.md",
        "LICENSE-MIT",
        "LICENSE-APACHE",
        "THIRD-PARTY-NOTICES.txt",
        "LICENSE-Geist-OFL.txt",
        ".background.tiff",
        ".VolumeIcon.icns",
        ".fseventsd",
    )
):
    icon_locations[name] = (100 + 150 * (column % 4), 560 + 150 * (column // 4))
