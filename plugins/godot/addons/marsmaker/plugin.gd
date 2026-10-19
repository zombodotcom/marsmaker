@tool
extends EditorPlugin
## Adds Project > Tools > "MarsMaker: add site..." which picks a site's meta.json and drops a
## MarsMakerTerrain node into the open scene. The MarsMakerTerrain node type itself is registered by
## its class_name, so it also appears in the "Add Node" dialog.

const MENU := "MarsMaker: add site..."
var _dialog: EditorFileDialog


func _enter_tree() -> void:
	add_tool_menu_item(MENU, _open_dialog)


func _exit_tree() -> void:
	remove_tool_menu_item(MENU)
	if _dialog:
		_dialog.queue_free()
		_dialog = null


func _open_dialog() -> void:
	if _dialog == null:
		_dialog = EditorFileDialog.new()
		_dialog.file_mode = EditorFileDialog.FILE_MODE_OPEN_FILE
		_dialog.access = EditorFileDialog.ACCESS_FILESYSTEM
		_dialog.title = "Select a MarsMaker site meta.json"
		_dialog.add_filter("meta.json", "MarsMaker site")
		_dialog.file_selected.connect(_on_meta_selected)
		EditorInterface.get_base_control().add_child(_dialog)
	_dialog.popup_file_dialog()


func _on_meta_selected(path: String) -> void:
	var root := EditorInterface.get_edited_scene_root()
	if root == null:
		push_warning("MarsMaker: open or create a 3D scene first")
		return
	var node := MarsMakerTerrain.new()
	node.name = path.get_base_dir().get_file()
	var ur := get_undo_redo()
	ur.create_action("Add MarsMaker site")
	ur.add_do_method(root, "add_child", node, true)
	ur.add_do_method(node, "set_owner", root)
	ur.add_do_property(node, "site_dir", path.get_base_dir())
	ur.add_do_reference(node)
	ur.add_undo_method(root, "remove_child", node)
	ur.commit_action()
	EditorInterface.edit_node(node)
