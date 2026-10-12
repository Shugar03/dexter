import gi

gi.require_version("Gtk", "3.0")
from gi.repository import Gtk

window = Gtk.Window(title="dexter-capture-fixture")
window.set_default_size(900, 200)
window.set_position(Gtk.WindowPosition.CENTER)

label = Gtk.Label()
label.set_markup('<span size="36000" weight="bold">DEXTER OCR PROBE</span>')
window.add(label)

window.connect("destroy", Gtk.main_quit)
window.show_all()
Gtk.main()
