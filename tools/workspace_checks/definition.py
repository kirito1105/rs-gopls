"""Check definitions and type definitions across packages and overlays."""

import os

from lsp_harness import session, site, write

from ._assertions import check_location, check_type_location, no_definition


def check_test_packages(executable_path, root, timeout):
    write(root / "go.mod", "module example.test/widget\n\ngo 1.22\n")
    implementation = write(root / "widget.go", "package widget\n\nfunc private() {}\nfunc Exported() {}\n")
    test_helper = write(root / "helper_test.go", "package widget\n\nfunc testOnly() {}\n")
    internal_test = write(
        root / "widget_test.go",
        'package widget\nimport "testing"\n'
        "func TestInternal(t *testing.T) { private(); testOnly() }\n",
    )
    external_test = write(
        root / "external_test.go",
        'package widget_test\nimport ("testing"; "example.test/widget")\n'
        "func TestExternal(t *testing.T) { widget.Exported() }\n",
    )
    private = site(internal_test, "{ [[private]]();")
    test_only = site(internal_test, "; [[testOnly]]()")
    public = site(external_test, "widget.[[Exported]]()")
    with session(executable_path, root, timeout) as client:
        client.open_document(internal_test, private.text)
        client.open_document(external_test, public.text)
        check_location(client, private, site(implementation, "func [[private]]()"))
        check_location(client, test_only, site(test_helper, "func [[testOnly]]()"))
        check_location(client, public, site(implementation, "func [[Exported]]()"))


def check_overlay_new_file(executable_path, root, timeout):
    write(root / "go.mod", "module example.test/overlay\n\ngo 1.22\n")
    caller = write(
        root / "caller.go",
        "package overlay\n\nfunc Existing() {}\nfunc Caller() { Existing(); Added() }\n",
    )
    internal = write(root / "internal_test.go", "package overlay\n\nfunc CallerTest() { Added() }\n")
    external = write(root / "external_test.go", "package overlay_test\n\nfunc CallerTest() { Added() }\n")
    internal_call = site(internal, "{ [[Added]]()")
    external_call = site(external, "{ [[Added]]()")
    virtual = root / "helper_extra.go"
    virtual_text = "package overlay\n\nfunc Added() { Existing() }\n"
    existing = site(caller, "func [[Existing]]()")
    existing_call = site(caller, "{ [[Existing]]();")
    added_call = site(caller, "; [[Added]]()")
    added = site(virtual, "func [[Added]]()", virtual_text)
    virtual_existing_call = site(virtual, "{ [[Existing]]()", virtual_text)
    with session(executable_path, root, timeout) as client:
        client.open_document(caller, existing.text)
        check_location(client, existing_call, existing)
        no_definition(client, added_call)
        client.open_document(virtual, virtual_text)
        check_location(client, added_call, added)
        check_location(client, internal_call, added)
        no_definition(client, external_call)
        check_location(client, virtual_existing_call, existing)
        client.close_document(virtual)
        no_definition(client, added_call)
        no_definition(client, internal_call)
    if virtual.exists():
        raise RuntimeError(f"test overlay unexpectedly wrote {virtual}")


def check_overlay_disk_restore(executable_path, root, timeout):
    write(root / "go.mod", "module example.test/overlayrestore\n\ngo 1.22\n")
    caller = write(
        root / "caller.go",
        'package overlayrestore\nimport "example.test/overlayrestore/dep"\n'
        "func Caller() { dep.Run() }\n",
    )
    saved_text = "package dep\n\n// marker\nfunc Run() {}\n"
    target_path = write(root / "dep" / "target.go", saved_text)
    reference = site(caller, "dep.[[Run]]()")
    saved = site(target_path, "func [[Run]]()")
    overlay = site(target_path, "func [[Run]]()", "// unsaved overlay\n" + saved_text)

    with session(executable_path, root, timeout) as client:
        client.open_document(caller, reference.text)
        check_location(client, reference, saved)

        client.open_document(target_path, saved.text)
        client.change_document(target_path, 2, overlay.text)
        check_location(client, reference, overlay)
        client.close_document(target_path)
        check_location(client, reference, saved)

        client.open_document(target_path, overlay.text)
        original_stat = target_path.stat()
        # Keep file size and modification time unchanged while moving the declaration.
        write(target_path, "package dep\n\nfunc Run() {}\n// marker\n")
        os.utime(target_path, ns=(original_stat.st_atime_ns, original_stat.st_mtime_ns))
        check_location(client, reference, overlay)
        client.close_document(target_path)
        check_location(client, reference, site(target_path, "func [[Run]]()"))


def check_type_overlay(executable_path, root, timeout):
    write(root / "go.mod", "module example.test/typeoverlay\n\ngo 1.22\n")
    caller = write(
        root / "caller.go",
        'package overlay\nimport "example.test/typeoverlay/dep"\n'
        "func call() {\n"
        "    value := dep.New()\n"
        "    /* \U0001f9ea */ _ = value\n"
        "    {\n"
        "        var value dep.Shadow\n"
        "        _ = value\n"
        "    }\n"
        "}\n",
    )
    write(root / "dep" / "factory.go", "package dep\n\nfunc New() *Item { return nil }\n")
    types = write(root / "dep" / "types.go", "package dep\n\ntype Item struct{}\ntype Shadow struct{}\n")
    inferred = site(caller, "*/ _ = [[value]]")
    shadowed = site(caller, "\n        _ = [[value]]")
    inferred_binding = site(caller, "[[value]] := dep.New()")
    shadowed_binding = site(caller, "var [[value]] dep.Shadow")
    item = site(types, "type [[Item]] struct{}")
    shadow = site(types, "type [[Shadow]] struct{}")

    with session(executable_path, root, timeout) as client:
        client.open_document(caller, inferred.text)
        check_location(client, inferred, inferred_binding)
        check_location(client, shadowed, shadowed_binding)
        check_type_location(client, inferred, item)
        check_type_location(client, shadowed, shadow)

        client.open_document(types, item.text)
        prefix = "// moved in the open buffer\n"
        start = {"line": 0, "character": 0}
        client.change_document_range(types, 2, start, start, prefix)
        edited = prefix + item.text
        check_type_location(client, inferred, site(types, "type [[Item]] struct{}", edited))
        check_type_location(client, shadowed, site(types, "type [[Shadow]] struct{}", edited))

        client.close_document(types)
        check_type_location(client, inferred, item)
        check_type_location(client, shadowed, shadow)


CASES = [
    ("test-packages", check_test_packages, 3),
    ("overlay-new-file", check_overlay_new_file, 6),
    ("overlay-disk-restore", check_overlay_disk_restore, 7),
    ("type-overlay", check_type_overlay, 8),
]
