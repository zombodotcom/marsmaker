@tool
@icon("res://addons/marsmaker/icon.svg")
class_name MarsMakerTerrain
extends Node3D
## Drop this node in a scene, set `site_dir` to a MarsMaker site folder (the one with meta.json) and the
## terrain is built when the scene runs. In the editor it builds a coarse preview (`editor_mesh_step`).
## Generated tiles are not saved into the scene file; they are rebuilt from the site folder each load.

signal site_loaded(tile_count: int)

## Folder containing meta.json, e.g. D:/marsmaker_data/sites/PSP_007018_1255_PSP_007229_1255
@export_global_dir var site_dir: String = "":
	set(v):
		site_dir = v
		if is_inside_tree() and Engine.is_editor_hint():
			rebuild()
## Keep every Nth DTM post for the visual mesh at runtime (1 = full 1 m detail, ~8M triangles per tile).
@export_range(1, 64) var mesh_step: int = 4
## Coarser mesh used for the in-editor preview so the editor stays responsive.
@export_range(1, 64) var editor_mesh_step: int = 16
@export var collision: bool = true
## Collision heightmap sample spacing in DTM posts (snapped to a divisor of tile_px - 1, e.g. 2016).
@export_range(1, 64) var collision_step: int = 1
## Put the site's lowest point at Y = 0 instead of absolute Mars height.
@export var relative_heights: bool = false
## Put the middle of the tile grid at X = Z = 0 instead of the projection centre.
@export var center: bool = false
@export var textures: bool = true
## Use a_{row}_{col}.png colour albedo tiles instead of the grey HiRISE ortho when they exist.
@export var prefer_albedo: bool = false
@export var build_on_ready: bool = true
@export_tool_button("Rebuild", "Reload") var _rebuild_btn = rebuild

var meta: Dictionary = {}
var _content: Node3D


func _ready() -> void:
	if build_on_ready and not site_dir.is_empty():
		rebuild()


func options() -> Dictionary:
	return {
		"mesh_step": editor_mesh_step if Engine.is_editor_hint() else mesh_step,
		"collision": collision and not Engine.is_editor_hint(),
		"collision_step": collision_step,
		"relative": relative_heights,
		"center": center,
		"textures": textures,
		"prefer_albedo": prefer_albedo,
	}


func rebuild() -> void:
	clear()
	if site_dir.is_empty():
		return
	var t0 := Time.get_ticks_msec()
	meta = MarsMakerSite.load_meta(site_dir)
	_content = MarsMakerSite.load_site(site_dir, options())
	if _content == null:
		return
	add_child(_content)   # no owner: generated nodes are not written into the .tscn
	print("MarsMaker: %s -> %d tiles in %d ms" % [site_dir.get_file(), _content.get_child_count(),
			Time.get_ticks_msec() - t0])
	site_loaded.emit(_content.get_child_count())


func clear() -> void:
	if _content:
		if is_inside_tree():
			_content.queue_free()
		else:
			_content.free()
		_content = null


## Terrain height (Y) under a point given in this node's local X/Z, via a physics ray against the
## collision heightmaps (needs collision on). Returns NAN when nothing is hit.
func height_at(x: float, z: float) -> float:
	if not is_inside_tree():
		return NAN
	var space := get_world_3d().direct_space_state
	var from := to_global(Vector3(x, 100000.0, z))
	var to := to_global(Vector3(x, -100000.0, z))
	var hit := space.intersect_ray(PhysicsRayQueryParameters3D.create(from, to))
	return to_local(hit.position).y if hit else NAN
