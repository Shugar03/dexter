import gi

gi.require_version("Gtk", "3.0")
from gi.repository import Gtk

window = Gtk.Window(title="dexter-act-fixture")
window.set_default_size(420, 400)
grid = Gtk.Grid(column_spacing=8, row_spacing=8, margin=12)
window.add(grid)

status = Gtk.Label(label="Ready")
status.set_xalign(0)
grid.attach(status, 0, 0, 2, 1)

greet = Gtk.Button(label="Greet")
greet.connect("clicked", lambda _button: status.set_text("Hello"))
grid.attach(greet, 0, 1, 1, 1)

name = Gtk.Entry()
name.get_accessible().set_name("Name")
grid.attach(name, 1, 1, 1, 1)

agree = Gtk.CheckButton(label="Agree")
grid.attach(agree, 0, 2, 1, 1)

locked = Gtk.Button(label="Locked")
locked.set_sensitive(False)
grid.attach(locked, 1, 2, 1, 1)

grid.attach(Gtk.Button(label="OK"), 0, 3, 1, 1)
grid.attach(Gtk.Button(label="OK"), 1, 3, 1, 1)

adjustment = Gtk.Adjustment(
    value=1, lower=0, upper=100, step_increment=1, page_increment=10, page_size=0
)
amount = Gtk.SpinButton(adjustment=adjustment, climb_rate=1, digits=1)
amount.get_accessible().set_name("Amount")
grid.attach(amount, 0, 4, 1, 1)

remove_name = Gtk.Button(label="Remove Name")
remove_name.connect("clicked", lambda _button: grid.remove(name))
grid.attach(remove_name, 1, 4, 1, 1)

read_only = Gtk.Entry()
read_only.get_accessible().set_name("Read only")
read_only.set_editable(False)
grid.attach(read_only, 0, 5, 2, 1)

window.connect("destroy", Gtk.main_quit)
window.show_all()
Gtk.main()
