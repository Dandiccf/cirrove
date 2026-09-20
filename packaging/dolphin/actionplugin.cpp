#include "cirroveclient.h"

#include <KAbstractFileItemActionPlugin>
#include <KFileItem>
#include <KFileItemListProperties>
#include <KLocalizedString>
#include <KPluginFactory>
#include <QAction>
#include <QIcon>
#include <QJsonArray>
#include <QSet>
#include <QWidget>
#include <algorithm>

using namespace CirroveDolphin;

class CirroveFileItemActionPlugin final : public KAbstractFileItemActionPlugin {
  Q_OBJECT

public:
  explicit CirroveFileItemActionPlugin(QObject *parent, const QVariantList &)
      : KAbstractFileItemActionPlugin(parent) {}

  QList<QAction *> actions(const KFileItemListProperties &properties,
                           QWidget *parentWidget) override {
    const auto status = requestSync(QStringLiteral("status"));
    if (!status) {
      return {};
    }
    const auto mounts = mountsFromStatus(*status);
    struct Selection {
      QString relative;
      bool directory;
    };
    QList<Selection> selection;
    QString label;
    for (const auto &item : properties.items()) {
      if (!item.url().isLocalFile()) {
        return {};
      }
      const auto found = locate(mounts, item.url().toLocalFile());
      if (!found || (!label.isEmpty() && found->label != label)) {
        return {};
      }
      label = found->label;
      selection.push_back(Selection{found->relative, item.isDir()});
    }
    if (selection.isEmpty()) {
      return {};
    }

    QJsonArray paths;
    for (const auto &item : selection) {
      paths.append(item.relative);
    }
    const auto reply = requestSync(
        QStringLiteral("paths"),
        {{QStringLiteral("label"), label}, {QStringLiteral("paths"), paths}});
    if (!reply) {
      return {};
    }
    const auto states = statesFromReply(*reply);
    if (states.size() != selection.size() ||
        std::any_of(states.cbegin(), states.cend(), [](const PathState &state) {
          return !state.refusal.isEmpty();
        })) {
      return {};
    }
    const bool unpin = shouldUnpin(states);
    const bool foldersOnly =
        std::all_of(selection.cbegin(), selection.cend(),
                    [](const Selection &item) { return item.directory; });
    QString text;
    if (unpin) {
      text = i18n("Stop keeping offline");
    } else if (foldersOnly && selection.size() == 1) {
      text = i18n("Keep folder offline");
    } else if (foldersOnly) {
      text = i18n("Keep folders offline");
    } else {
      text = i18n("Keep offline");
    }

    auto *action = new QAction(
        QIcon::fromTheme(QStringLiteral("io.github.Dandiccf.Cirrove-symbolic")),
        text, parentWidget);
    connect(action, &QAction::triggered, this, [this, label, selection, unpin] {
      for (const auto &item : selection) {
        const auto verb =
            unpin ? QStringLiteral("unpin") : QStringLiteral("pin");
        requestAsync(
            this, verb, pinBody(label, item.relative, item.directory),
            [this](std::optional<QJsonObject> response) {
              if (!response) {
                Q_EMIT error(i18n("Cirrove service is not reachable"));
                return;
              }
              const auto refusal =
                  response->value(QStringLiteral("refusal")).toString();
              if (!refusal.isEmpty()) {
                Q_EMIT error(
                    i18n("Cirrove could not change offline availability"));
              }
            });
      }
    });
    return QList<QAction *>{action};
  }
};

K_PLUGIN_CLASS_WITH_JSON(CirroveFileItemActionPlugin,
                         "cirrovefileitemaction.json")

#include "actionplugin.moc"
