"""Platform-neutral scenario steps using the Windows/macOS Ui contract.

Keep selectors as locale keys. Installation, process lifetime and assertions
about native services stay in the platform harness.
"""


def wizard_detect(ui):
    ui.wait(key="wizard.detect.title", timeout=30)


def wizard_skip(ui):
    wizard_detect(ui)
    ui.click(key="wizard.skip")
    ui.wait_gone(key="wizard.detect.title")


def wizard_custom(ui, name, base_url, key):
    wizard_detect(ui)
    ui.click(key="wizard.continue")
    ui.wait(key="wizard.provider.title")
    ui.act("select", key="wizard.preset.custom.title", type="RadioButton")
    ui.click(key="wizard.continue")
    ui.wait(key="wizard.credentials.custom.title")
    for label, value in (("name", name), ("url", base_url), ("key", key)):
        ui.type_into(value, key=f"wizard.custom.{label}", type="Edit")
    ui.click(key="wizard.continue")
    ui.wait(key="wizard.custom.test.title", timeout=90)
    ui.click(key="wizard.continue", timeout=90)
    ui.wait(key="wizard.review.title")
    ui.click(key="wizard.apply")
    ui.wait_gone(key="wizard.review.title", timeout=120)


def router_page(ui):
    ui.page("router")


def claude_page(ui):
    ui.page("claude-code")


def notification_page(ui):
    ui.superba("notify")
