import gi

gi.require_version("Gtk", "3.0")
from gi.repository import Gdk, Gtk

window = Gtk.Window(title="dexter-act-fixture")
window.set_default_size(500, 560)
grid = Gtk.Grid(column_spacing=8, row_spacing=8, margin=12)
window.add(grid)

status = Gtk.Label(label="Ready")
status.set_xalign(0)
grid.attach(status, 0, 0, 2, 1)

greet = Gtk.Button(label="Greet")
greet.connect("clicked", lambda _button: status.set_text("Hello"))


def record_mouse_press(_button, event):
    status.set_text(f"Pressed button {event.button}")
    return False


greet.add_events(Gdk.EventMask.BUTTON_PRESS_MASK)
greet.connect("button-press-event", record_mouse_press)
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

scroll = Gtk.ScrolledWindow()
scroll.set_policy(Gtk.PolicyType.NEVER, Gtk.PolicyType.AUTOMATIC)
scroll.set_min_content_height(160)
scroll.get_accessible().set_name("Scroll area")
scroll_content = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=4)
for index in range(40):
    scroll_content.pack_start(Gtk.Label(label=f"Scroll row {index}"), False, False, 0)
scroll.add(scroll_content)
grid.attach(scroll, 0, 6, 2, 1)

second = Gtk.Window(title="dexter-act-fixture-second")
second.set_default_size(220, 160)
second.add(Gtk.Label(label="Second window"))
second.move(520, 80)
second.show_all()

window.connect("destroy", Gtk.main_quit)
window.show_all()
Gtk.main()
