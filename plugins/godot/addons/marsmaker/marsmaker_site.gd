class_name MarsMakerSite
extends RefCounted
## Loads a MarsMaker site folder (meta.json + h_{row}_{col}.png heights + t_/a_ textures) as Godot terrain.
##
## Coordinates: 1 unit = 1 m, +X = east, +Y = up, +Z = south (north = -Z). The origin is the site's local
## projection centre (meta.json "projection"), or the middle of the tile grid when `center` is on.
## Heights are absolute Mars areoid heights unless `relative` is on (site minimum at Y = 0).
##
## Works at runtime and in the editor; needs no import step. Site folders may live anywhere on disk
## (absolute paths such as "D:/marsmaker_data/sites/<site>") or under res:// / user://.

const MARS_TINT := Color(1.0, 0.62, 0.42)
const NO_TEXTURE_COLOR := Color(0.55, 0.36, 0.25)

## Options understood by load_site() / build_tile(). Copy and change what you need.
static func default_options() -> Dictionary:
	return {
		"mesh_step": 4,          # keep every Nth DTM post for the visual mesh (1 = full 1 m detail)
		"collision": true,       # add a StaticBody3D + HeightMapShape3D per tile
		"collision_step": 1,     # sample spacing of the collision heightmap (snapped to a divisor of tile_px-1)
		"relative": false,       # true: site minimum height -> Y = 0
		"center": false,         # true: middle of the tile grid -> X = Z = 0
		"textures": true,
		"prefer_albedo": false,  # use the de-lit a_{r}_{c}.png albedo instead of the t_ ortho when present
		"tint_grey": true,       # multiply greyscale textures (t_ and a_) by a Mars dust colour
		"tiles": [],             # optional list of Vector2i(row, col) to load; empty = all
	}


static func load_meta(site_dir: String) -> Dictionary:
	var txt := FileAccess.get_file_as_string(site_dir.path_join("meta.json"))
	if txt.is_empty():
		push_error("MarsMaker: cannot read %s/meta.json" % site_dir)
		return {}
	var meta = JSON.parse_string(txt)
	return meta if meta is Dictionary else {}


## Builds every tile under a new Node3D and returns it (not yet added to the tree).
static func load_site(site_dir: String, opts: Dictionary = {}) -> Node3D:
	var o := default_options()
	o.merge(opts, true)
	var meta := load_meta(site_dir)
	if meta.is_empty():
		return null
	var root := Node3D.new()
	root.name = site_dir.get_file()
	for k in ["id", "center_lat", "center_lon_east", "projection", "height_min_m", "height_max_m"]:
		root.set_meta(k, meta.get(k))
	var y_off: float = meta["height_min_m"] if o["relative"] else 0.0
	root.set_meta("y_offset_m", y_off)
	var origin := site_origin(meta, o["center"])
	root.set_meta("origin_east_north_m", origin)
	var want: Array = o["tiles"]
	for t in meta["tiles"]:
		var rc := Vector2i(int(t["y"]), int(t["x"]))
		if not want.is_empty() and not want.has(rc):
			continue
		var tile := build_tile(site_dir, meta, t, o, origin, y_off)
		if tile:
			root.add_child(tile)
	return root


static func site_origin(meta: Dictionary, center: bool) -> Vector2:
	if not center:
		return Vector2.ZERO
	var size: float = meta["tile_size_m"]
	var g: Dictionary = meta["grid_top_left_m"]
	return Vector2(g["east"] + meta["grid_tiles"][0] * size * 0.5, g["north"] - meta["grid_tiles"][1] * size * 0.5)


## Builds one tile: Node3D at the tile's north-west corner with a MeshInstance3D and optional collision.
static func build_tile(site_dir: String, meta: Dictionary, t: Dictionary, o: Dictionary,
		origin: Vector2, y_off: float) -> Node3D:
	var row := int(t["y"])
	var col := int(t["x"])
	var hm := read_height_png(site_dir.path_join(t["height"]))
	if hm.is_empty():
		return null
	var node := Node3D.new()
	node.name = "h_%d_%d" % [row, col]
	var tl: Dictionary = t["top_left_m"]
	node.position = Vector3(float(tl["east"]) - origin.x, 0.0, -(float(tl["north"]) - origin.y))

	var mesh := build_mesh(hm, meta, int(o["mesh_step"]), y_off)
	if mesh == null:
		node.free()
		return null
	var mi := MeshInstance3D.new()
	mi.name = "mesh"
	mi.mesh = mesh
	mi.material_override = make_material(site_dir, t, row, col, o)
	node.add_child(mi)
	node.set_meta("height_range", mesh.get_meta("height_range"))
	node.set_meta("vertex_count", mesh.get_meta("vertex_count"))

	if o["collision"]:
		var body := build_collision(hm, meta, int(o["collision_step"]), y_off)
		if body:
			node.add_child(body)
	return node


# ------------------------------------------------------------------------------------------- PNG reading
## Godot's PNG loader reduces 16-bit images to 8 bits, so the height PNG is re-labelled as 8-bit
## grey+alpha (same byte layout: 2 bytes per pixel, identical filter maths) and decoded natively.
## Returns {w, h, data: PackedByteArray big-endian uint16 pairs} or {} on failure.
static func read_height_png(path: String) -> Dictionary:
	var buf := FileAccess.get_file_as_bytes(path)
	if buf.size() < 33 or buf.slice(12, 16).get_string_from_ascii() != "IHDR":
		push_error("MarsMaker: %s is not a PNG" % path)
		return {}
	var w := buf.decode_u32(16)   # decode_* is little-endian; PNG is big-endian
	w = _bswap32(w)
	var h := _bswap32(buf.decode_u32(20))
	var depth := buf[24]
	var ctype := buf[25]
	if buf[28] != 0:
		push_error("MarsMaker: interlaced height PNGs are not supported: %s" % path)
		return {}
	if depth == 16 and ctype == 0:
		buf[24] = 8
		buf[25] = 4
		var crc := _crc32(buf.slice(12, 29))
		buf[29] = (crc >> 24) & 0xFF
		buf[30] = (crc >> 16) & 0xFF
		buf[31] = (crc >> 8) & 0xFF
		buf[32] = crc & 0xFF
	elif not (depth == 8 and ctype == 4):
		push_error("MarsMaker: %s is not a 16-bit greyscale PNG (depth %d, type %d)" % [path, depth, ctype])
		return {}
	var img := Image.new()
	if img.load_png_from_buffer(buf) != OK or img.get_format() != Image.FORMAT_LA8:
		push_error("MarsMaker: failed to decode %s" % path)
		return {}
	return {"w": w, "h": h, "data": img.get_data()}


static func _bswap32(v: int) -> int:
	return ((v & 0xFF) << 24) | ((v >> 8 & 0xFF) << 16) | ((v >> 16 & 0xFF) << 8) | (v >> 24 & 0xFF)


static var _crc_table := PackedInt64Array()

static func _crc32(bytes: PackedByteArray) -> int:
	if _crc_table.is_empty():
		_crc_table.resize(256)
		for n in 256:
			var c := n
			for k in 8:
				c = (0xEDB88320 ^ (c >> 1)) if (c & 1) else (c >> 1)
			_crc_table[n] = c
	var crc := 0xFFFFFFFF
	for b in bytes:
		crc = _crc_table[(crc ^ b) & 0xFF] ^ (crc >> 8)
	return crc ^ 0xFFFFFFFF


## Sample positions 0, step, 2*step ... plus the last pixel, so tile edges always meet their neighbour.
static func sample_indices(n_px: int, step: int) -> PackedInt32Array:
	var idx := PackedInt32Array()
	var i := 0
	while i < n_px:
		idx.append(i)
		i += step
	if idx[idx.size() - 1] != n_px - 1:
		idx.append(n_px - 1)
	return idx


# ------------------------------------------------------------------------------------------- mesh
## Height of a raw sample: v in 1..65535 -> height_min + (v-1)*range/65534; v = 0 is nodata.
static func build_mesh(hm: Dictionary, meta: Dictionary, step: int, y_off: float) -> ArrayMesh:
	step = maxi(step, 1)
	var W: int = hm["w"]
	var H: int = hm["h"]
	var data: PackedByteArray = hm["data"]
	var hmin: float = meta["height_min_m"]
	var k: float = float(meta["height_range_m"]) / 65534.0
	var mpp: float = meta["metres_per_pixel"]
	var ri := sample_indices(H, step)
	var ci := sample_indices(W, step)
	var nr := ri.size()
	var nc := ci.size()

	# sampled heights (NAN = nodata)
	var hs := PackedFloat32Array()
	hs.resize(nr * nc)
	var i := 0
	for r in nr:
		var base := ri[r] * W
		for c in nc:
			var p := (base + ci[c]) * 2
			var v := (data[p] << 8) | data[p + 1]
			hs[i] = (hmin + (v - 1) * k - y_off) if v > 0 else NAN
			i += 1

	# quads with 4 valid corners; vertices used by at least one of them
	var qok := PackedByteArray()
	qok.resize((nr - 1) * (nc - 1))
	var used := PackedByteArray()
	used.resize(nr * nc)
	var nq := 0
	for r in nr - 1:
		for c in nc - 1:
			var a := r * nc + c
			if not (is_nan(hs[a]) or is_nan(hs[a + 1]) or is_nan(hs[a + nc]) or is_nan(hs[a + nc + 1])):
				qok[r * (nc - 1) + c] = 1
				used[a] = 1
				used[a + 1] = 1
				used[a + nc] = 1
				used[a + nc + 1] = 1
				nq += 1
	if nq == 0:
		return null

	var vid := PackedInt32Array()
	vid.resize(nr * nc)
	var verts := PackedVector3Array()
	var norms := PackedVector3Array()
	var uvs := PackedVector2Array()
	var lo := INF
	var hi := -INF
	for r in nr:
		var z := ri[r] * mpp
		var rn := maxi(r - 1, 0)
		var rs := mini(r + 1, nr - 1)
		var dz := (ri[rs] - ri[rn]) * mpp
		var vv := (ri[r] + 0.5) / H
		for c in nc:
			var a := r * nc + c
			if not used[a]:
				vid[a] = -1
				continue
			vid[a] = verts.size()
			var y := hs[a]
			lo = minf(lo, y)
			hi = maxf(hi, y)
			verts.append(Vector3(ci[c] * mpp, y, z))
			uvs.append(Vector2((ci[c] + 0.5) / W, vv))
			# central-difference normal; nodata neighbours fall back to the centre height
			var cw := maxi(c - 1, 0)
			var ce := mini(c + 1, nc - 1)
			var hw := hs[r * nc + cw]
			var he := hs[r * nc + ce]
			var hn := hs[rn * nc + c]
			var hsouth := hs[rs * nc + c]
			if is_nan(hw): hw = y
			if is_nan(he): he = y
			if is_nan(hn): hn = y
			if is_nan(hsouth): hsouth = y
			var dx := (ci[ce] - ci[cw]) * mpp
			norms.append(Vector3(-(he - hw) / dx, 1.0, -(hsouth - hn) / dz).normalized())

	# Godot front faces are clockwise seen from the front (+Y here): NW-SE-SW, NW-NE-SE
	var idx := PackedInt32Array()
	idx.resize(nq * 6)
	var j := 0
	for r in nr - 1:
		for c in nc - 1:
			if not qok[r * (nc - 1) + c]:
				continue
			var a := r * nc + c
			var nw := vid[a]
			var ne := vid[a + 1]
			var sw := vid[a + nc]
			var se := vid[a + nc + 1]
			idx[j] = nw; idx[j + 1] = se; idx[j + 2] = sw
			idx[j + 3] = nw; idx[j + 4] = ne; idx[j + 5] = se
			j += 6

	var arrays := []
	arrays.resize(Mesh.ARRAY_MAX)
	arrays[Mesh.ARRAY_VERTEX] = verts
	arrays[Mesh.ARRAY_NORMAL] = norms
	arrays[Mesh.ARRAY_TEX_UV] = uvs
	arrays[Mesh.ARRAY_INDEX] = idx
	var mesh := ArrayMesh.new()
	mesh.add_surface_from_arrays(Mesh.PRIMITIVE_TRIANGLES, arrays)
	mesh.set_meta("height_range", Vector2(lo, hi))
	mesh.set_meta("vertex_count", verts.size())
	return mesh


# ------------------------------------------------------------------------------------------- collision
## StaticBody3D with a HeightMapShape3D. HeightMapShape3D has 1-unit sample spacing and is centred on
## its node, so the shape is scaled uniformly by the step (heights pre-divided) and offset to the tile
## centre. Nodata samples are filled with the tile's lowest valid height (HeightMapShape3D has no holes).
static func build_collision(hm: Dictionary, meta: Dictionary, step: int, y_off: float) -> StaticBody3D:
	var W: int = hm["w"]
	var H: int = hm["h"]
	step = snap_step(mini(W, H) - 1, step)
	var data: PackedByteArray = hm["data"]
	var hmin: float = meta["height_min_m"]
	var k: float = float(meta["height_range_m"]) / 65534.0
	var mpp: float = meta["metres_per_pixel"]
	var nc := (W - 1) / step + 1
	var nr := (H - 1) / step + 1
	var md := PackedFloat32Array()
	md.resize(nr * nc)
	var inv := 1.0 / (step * mpp)
	var lo := INF
	var hi := -INF
	var holes := PackedInt32Array()
	var i := 0
	for r in nr:
		var base := r * step * W
		for c in nc:
			var p := (base + c * step) * 2
			var v := (data[p] << 8) | data[p + 1]
			if v > 0:
				var y := hmin + (v - 1) * k - y_off
				lo = minf(lo, y)
				hi = maxf(hi, y)
				md[i] = y * inv
			else:
				holes.append(i)
			i += 1
	if lo == INF:
		return null
	for hidx in holes:
		md[hidx] = lo * inv
	var shape := HeightMapShape3D.new()
	shape.map_width = nc
	shape.map_depth = nr
	shape.map_data = md
	var cs := CollisionShape3D.new()
	cs.name = "heightmap"
	cs.shape = shape
	var s := step * mpp
	cs.scale = Vector3(s, s, s)
	cs.position = Vector3((nc - 1) * s * 0.5, 0.0, (nr - 1) * s * 0.5)
	var body := StaticBody3D.new()
	body.name = "collision"
	body.add_child(cs)
	body.set_meta("height_range", Vector2(lo, hi))
	body.set_meta("step", step)
	body.set_meta("nodata_filled", holes.size())
	return body


## Largest divisor of n that is <= step (HeightMapShape3D needs evenly spaced samples).
static func snap_step(n: int, step: int) -> int:
	step = clampi(step, 1, n)
	while n % step != 0:
		step -= 1
	return step


# ------------------------------------------------------------------------------------------- material
static func make_material(site_dir: String, t: Dictionary, row: int, col: int, o: Dictionary) -> StandardMaterial3D:
	var mat := StandardMaterial3D.new()
	mat.roughness = 0.95
	mat.metallic = 0.0
	mat.albedo_color = NO_TEXTURE_COLOR
	if not o["textures"]:
		return mat
	var cands := []
	if o["prefer_albedo"]:
		cands.append([site_dir.path_join("a_%d_%d.png" % [row, col]), true])
	var tname: String = t.get("texture", "t_%d_%d.png" % [row, col])
	cands.append([site_dir.path_join(tname), false])
	for cand in cands:
		if not FileAccess.file_exists(cand[0]):
			continue
		var img := Image.load_from_file(cand[0])
		if img == null or img.is_empty():
			continue
		img.generate_mipmaps()
		if _image_empty(img):
			# converter writes an all-zero texture when the ortho could not be aligned
			return mat
		mat.albedo_texture = ImageTexture.create_from_image(img)
		var grey := img.get_format() == Image.FORMAT_L8 or img.get_format() == Image.FORMAT_LA8
		mat.albedo_color = MARS_TINT if (grey and o["tint_grey"]) else Color.WHITE
		mat.texture_filter = BaseMaterial3D.TEXTURE_FILTER_LINEAR_WITH_MIPMAPS_ANISOTROPIC
		mat.texture_repeat = false
		return mat
	return mat


## True when every pixel is 0: checks a small box-filtered mip level instead of millions of pixels.
static func _image_empty(img: Image) -> bool:
	var level := 0
	while level < img.get_mipmap_count() and max(img.get_width() >> level, img.get_height() >> level) > 32:
		level += 1
	var start := img.get_mipmap_offset(level)
	var data := img.get_data()
	var end := data.size() if level >= img.get_mipmap_count() else img.get_mipmap_offset(level + 1)
	for b in data.slice(start, end):
		if b != 0:
			return false
	return true
