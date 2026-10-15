import { Component, type ReactNode } from "react";
import { t } from "../i18n";

/** A failing panel shows its own error instead of blanking the page. */
export class PanelBoundary extends Component<{ children: ReactNode }, { failed: boolean }> {
  override state = { failed: false };
  static getDerivedStateFromError() { return { failed: true }; }
  override render() {
    if (!this.state.failed) return this.props.children;
    return <div className="panel-error" role="alert"><p>{t("error.panel")}</p><button type="button" className="btn" onClick={() => this.setState({ failed: false })}>{t("error.reload")}</button></div>;
  }
}
