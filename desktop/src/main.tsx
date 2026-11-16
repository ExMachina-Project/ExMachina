import React from "react";
import { createRoot } from "react-dom/client";
import { App, ConfigProvider, theme } from "antd";
import zhCN from "antd/locale/zh_CN";
import { DesktopApp } from "./DesktopApp";
import "../../webui/src/styles.css";
import "./styles.css";

createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <ConfigProvider locale={zhCN} button={{ autoInsertSpace: false }} theme={{ algorithm: theme.darkAlgorithm, token: {
      colorPrimary: "#9a91ff", colorBgBase: "#101116", colorBgContainer: "#171920", colorBgElevated: "#20232c",
      colorBorder: "#303441", colorText: "#e4e7ef", borderRadius: 7, fontSize: 13,
      fontFamily: '"Inter", "Segoe UI", "PingFang SC", system-ui, sans-serif',
    } }}>
      <App><DesktopApp /></App>
    </ConfigProvider>
  </React.StrictMode>,
);
