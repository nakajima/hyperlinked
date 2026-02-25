//
//  ShareViewController.swift
//  Add
//
//  Created by Pat Nakajima on 2/24/26.
//

import UIKit

final class ShareViewController: UIViewController {
    private enum SaveExecutionResult {
        case delivered
        case queuedOffline
        case failed(String)
    }

    private let appGroupID = "group.fm.folder.hyperlinked"
    private let selectedServerURLKey = "selected_server_base_url"

    private var candidates: [SharedLinkCandidate] = []
    private var selectedCandidateID: String?
    private var extractedTitle = ""
    private var fallbackErrorMessage: String?
    private var isPreparingPayload = false
    private var isSubmitting = false
    private var isManualUIVisible = false
    private var isShowingTransientToast = false
    private lazy var outboxStore: ShareOutboxStore? = try? ShareOutboxStore.openShared(
        appGroupID: appGroupID
    )

    private let titleLabel = UILabel()
    private let titleTextView = UITextView()
    private let titlePlaceholderLabel = UILabel()
    private let titleContainerView = UIView()
    private let linksLabel = UILabel()
    private let linksTableView = UITableView(frame: .zero, style: .plain)
    private let statusLabel = UILabel()
    private let activityIndicator = UIActivityIndicatorView(style: .medium)
    private let cancelButton = UIButton(type: .system)
    private let saveButton = UIButton(type: .system)
    private var rootStackView: UIStackView?
    private var transientToastView: UIView?
    private var linksTableHeightConstraint: NSLayoutConstraint?
    private var titleHeightConstraint: NSLayoutConstraint?
    private var minTitleHeight: CGFloat = 0
    private var maxTitleHeight: CGFloat = 0

    private var selectedCandidate: SharedLinkCandidate? {
        guard let selectedCandidateID else {
            return nil
        }
        return candidates.first(where: { $0.id == selectedCandidateID })
    }

    override func viewDidLoad() {
        super.viewDidLoad()
        configureUI()
        setManualUIVisible(false)
        preparePayload()
    }

    override func viewDidLayoutSubviews() {
        super.viewDidLayoutSubviews()
        guard isManualUIVisible else {
            preferredContentSize = isShowingTransientToast
                ? CGSize(width: 220, height: 90)
                : CGSize(width: 1, height: 1)
            return
        }
        updateTitleHeight()
        let fittingSize = CGSize(width: view.bounds.width - 32, height: UIView.layoutFittingCompressedSize.height)
        let targetHeight = view.systemLayoutSizeFitting(
            fittingSize,
            withHorizontalFittingPriority: .required,
            verticalFittingPriority: .fittingSizeLevel
        ).height + 16
        preferredContentSize = CGSize(width: view.bounds.width, height: min(max(targetHeight, 260), 460))
    }

    private func configureUI() {
        view.backgroundColor = .systemBackground

        titleLabel.text = "Save to Hyperlinked"
        titleLabel.font = .preferredFont(forTextStyle: .headline)

        let titleFont = UIFont.preferredFont(forTextStyle: .body)
        titleTextView.font = titleFont
        titleTextView.backgroundColor = .secondarySystemBackground
        titleTextView.textColor = .label
        titleTextView.autocorrectionType = .no
        titleTextView.autocapitalizationType = .sentences
        titleTextView.layer.cornerRadius = 8
        titleTextView.layer.borderWidth = 1
        titleTextView.layer.borderColor = UIColor.separator.cgColor
        titleTextView.textContainerInset = UIEdgeInsets(top: 10, left: 8, bottom: 10, right: 8)
        titleTextView.isScrollEnabled = false
        titleTextView.delegate = self
        titleTextView.translatesAutoresizingMaskIntoConstraints = false

        minTitleHeight = ceil(titleFont.lineHeight + titleTextView.textContainerInset.top + titleTextView.textContainerInset.bottom)
        maxTitleHeight = ceil((titleFont.lineHeight * 4) + titleTextView.textContainerInset.top + titleTextView.textContainerInset.bottom)

        titlePlaceholderLabel.text = "Title (optional)"
        titlePlaceholderLabel.font = .preferredFont(forTextStyle: .body)
        titlePlaceholderLabel.textColor = .placeholderText
        titlePlaceholderLabel.translatesAutoresizingMaskIntoConstraints = false

        titleContainerView.translatesAutoresizingMaskIntoConstraints = false
        titleContainerView.addSubview(titleTextView)
        titleContainerView.addSubview(titlePlaceholderLabel)
        let titleHeightConstraint = titleTextView.heightAnchor.constraint(equalToConstant: minTitleHeight)
        titleHeightConstraint.isActive = true
        self.titleHeightConstraint = titleHeightConstraint
        NSLayoutConstraint.activate([
            titleTextView.topAnchor.constraint(equalTo: titleContainerView.topAnchor),
            titleTextView.leadingAnchor.constraint(equalTo: titleContainerView.leadingAnchor),
            titleTextView.trailingAnchor.constraint(equalTo: titleContainerView.trailingAnchor),
            titleTextView.bottomAnchor.constraint(equalTo: titleContainerView.bottomAnchor),
            titlePlaceholderLabel.leadingAnchor.constraint(equalTo: titleTextView.leadingAnchor, constant: 14),
            titlePlaceholderLabel.topAnchor.constraint(equalTo: titleTextView.topAnchor, constant: 12),
        ])

        linksLabel.text = "Links"
        linksLabel.font = .preferredFont(forTextStyle: .subheadline)
        linksLabel.textColor = .secondaryLabel

        linksTableView.dataSource = self
        linksTableView.delegate = self
        linksTableView.separatorStyle = .none
        linksTableView.rowHeight = 60
        linksTableView.layer.cornerRadius = 12
        linksTableView.layer.masksToBounds = true
        linksTableView.backgroundColor = .secondarySystemBackground
        linksTableView.alwaysBounceVertical = false

        statusLabel.font = .preferredFont(forTextStyle: .footnote)
        statusLabel.textColor = .secondaryLabel
        statusLabel.numberOfLines = 2

        cancelButton.setTitle("Cancel", for: .normal)
        cancelButton.addTarget(self, action: #selector(cancelTapped), for: .touchUpInside)
        cancelButton.configuration = .bordered()

        saveButton.setTitle("Save", for: .normal)
        saveButton.addTarget(self, action: #selector(saveTapped), for: .touchUpInside)
        saveButton.configuration = .filled()

        let buttonStack = UIStackView(arrangedSubviews: [cancelButton, saveButton])
        buttonStack.axis = .horizontal
        buttonStack.spacing = 12
        buttonStack.distribution = .fillEqually

        let statusStack = UIStackView(arrangedSubviews: [activityIndicator, statusLabel])
        statusStack.axis = .horizontal
        statusStack.alignment = .center
        statusStack.spacing = 8

        let rootStack = UIStackView(
            arrangedSubviews: [titleLabel, titleContainerView, linksLabel, linksTableView, statusStack, buttonStack]
        )
        rootStack.axis = .vertical
        rootStack.spacing = 12
        rootStack.translatesAutoresizingMaskIntoConstraints = false
        rootStackView = rootStack

        view.addSubview(rootStack)

        NSLayoutConstraint.activate([
            rootStack.topAnchor.constraint(equalTo: view.safeAreaLayoutGuide.topAnchor, constant: 16),
            rootStack.leadingAnchor.constraint(equalTo: view.leadingAnchor, constant: 16),
            rootStack.trailingAnchor.constraint(equalTo: view.trailingAnchor, constant: -16),
            rootStack.bottomAnchor.constraint(lessThanOrEqualTo: view.safeAreaLayoutGuide.bottomAnchor, constant: -16),
            saveButton.heightAnchor.constraint(equalToConstant: 44),
        ])

        let tableHeightConstraint = linksTableView.heightAnchor.constraint(equalToConstant: 48)
        tableHeightConstraint.isActive = true
        linksTableHeightConstraint = tableHeightConstraint
    }

    private func preparePayload() {
        isPreparingPayload = true
        fallbackErrorMessage = nil
        updateUIState()

        Task {
            let extraction = await SharePayloadExtractor.extract(from: extensionContext, composeText: nil)
            await MainActor.run {
                applyExtraction(extraction)
            }

            if extraction.candidates.count == 1, let candidate = extraction.candidates.first {
                let defaultTitle = extraction.title.trimmingCharacters(in: .whitespacesAndNewlines)
                let saveResult = await executeSave(
                    candidate: candidate,
                    resolvedTitle: defaultTitle.nilIfEmpty
                )
                switch saveResult {
                case .delivered:
                    await completeAfterToast("Saved")
                    return
                case .queuedOffline:
                    await completeAfterToast("Saved offline")
                    return
                case .failed(let message):
                    await MainActor.run {
                        fallbackErrorMessage = message
                        isPreparingPayload = false
                        setManualUIVisible(true)
                        updateUIState()
                    }
                    return
                }
            }

            await MainActor.run {
                isPreparingPayload = false
                setManualUIVisible(true)
                updateUIState()
            }
        }
    }

    @MainActor
    private func applyExtraction(_ extraction: ShareExtractionResult) {
        extractedTitle = extraction.title
        if titleTextView.text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            titleTextView.text = extractedTitle
            updateTitlePlaceholderVisibility()
            updateTitleHeight()
        }

        candidates = extraction.candidates
        if candidates.count == 1 {
            selectedCandidateID = candidates.first?.id
        } else if !candidates.contains(where: { $0.id == selectedCandidateID }) {
            selectedCandidateID = nil
        }

        linksTableView.reloadData()
    }

    private func executeSave(
        candidate: SharedLinkCandidate,
        resolvedTitle: String?
    ) async -> SaveExecutionResult {
        guard let serverURL = configuredServerURL() else {
            return .failed("Open Hyperlinked and configure a server first.")
        }
        guard let outboxStore else {
            return .failed("Could not save this link locally. Please try again.")
        }

        let queuedItem: ShareOutboxItemRecord
        do {
            queuedItem = try outboxStore.enqueue(
                url: candidate.url.absoluteString,
                title: resolvedTitle ?? ""
            )
        } catch {
            return .failed("Could not save this link locally. Please try again.")
        }

        let client = ShareAPIClient(baseURL: serverURL)
        do {
            try await client.createHyperlink(title: queuedItem.title, url: queuedItem.url)
            try outboxStore.markDelivered(id: queuedItem.id)
            await drainPendingOutbox(store: outboxStore, client: client, excludingID: queuedItem.id)
            return .delivered
        } catch {
            try? outboxStore.markAttemptFailed(id: queuedItem.id, errorMessage: error.localizedDescription)
            await drainPendingOutbox(store: outboxStore, client: client, excludingID: queuedItem.id)
            return .queuedOffline
        }
    }

    @MainActor
    private func setManualUIVisible(_ visible: Bool) {
        isManualUIVisible = visible
        rootStackView?.isHidden = !visible
        view.backgroundColor = visible ? .systemBackground : .clear
        view.setNeedsLayout()
    }

    private func completeAfterToast(_ message: String) async {
        await MainActor.run {
            setManualUIVisible(false)
            showTransientToast(message)
        }
        try? await Task.sleep(nanoseconds: 700_000_000)
        await MainActor.run {
            hideTransientToast()
            extensionContext?.completeRequest(returningItems: [], completionHandler: nil)
        }
    }

    @MainActor
    private func showTransientToast(_ message: String) {
        transientToastView?.removeFromSuperview()
        isShowingTransientToast = true

        let blurView = UIVisualEffectView(effect: UIBlurEffect(style: .systemChromeMaterialDark))
        blurView.layer.cornerRadius = 16
        blurView.clipsToBounds = true
        blurView.translatesAutoresizingMaskIntoConstraints = false
        blurView.alpha = 0

        let label = UILabel()
        label.text = message
        label.font = .preferredFont(forTextStyle: .subheadline)
        label.textColor = .white
        label.translatesAutoresizingMaskIntoConstraints = false

        blurView.contentView.addSubview(label)
        NSLayoutConstraint.activate([
            label.topAnchor.constraint(equalTo: blurView.contentView.topAnchor, constant: 10),
            label.bottomAnchor.constraint(equalTo: blurView.contentView.bottomAnchor, constant: -10),
            label.leadingAnchor.constraint(equalTo: blurView.contentView.leadingAnchor, constant: 14),
            label.trailingAnchor.constraint(equalTo: blurView.contentView.trailingAnchor, constant: -14),
        ])

        view.addSubview(blurView)
        NSLayoutConstraint.activate([
            blurView.centerXAnchor.constraint(equalTo: view.centerXAnchor),
            blurView.centerYAnchor.constraint(equalTo: view.centerYAnchor),
        ])
        view.layoutIfNeeded()
        transientToastView = blurView

        UIView.animate(withDuration: 0.16) {
            blurView.alpha = 1
        }
    }

    @MainActor
    private func hideTransientToast() {
        isShowingTransientToast = false
        transientToastView?.removeFromSuperview()
        transientToastView = nil
    }

    private func validationMessage() -> String? {
        if isSubmitting {
            return "Saving..."
        }
        if isPreparingPayload {
            return "Preparing shared content..."
        }
        guard configuredServerURL() != nil else {
            return "Open Hyperlinked and configure a server first."
        }
        guard outboxStore != nil else {
            return "Could not access shared storage for offline queue."
        }
        guard !candidates.isEmpty else {
            return "No URL found in the shared content."
        }
        guard selectedCandidate != nil else {
            return "Choose a URL to save."
        }
        return nil
    }

    private func updateUIState() {
        let validationIssue = validationMessage()
        let statusMessage = validationIssue ?? fallbackErrorMessage
        let isReady = validationIssue == nil

        statusLabel.text = statusMessage
        statusLabel.isHidden = statusMessage == nil
        if validationIssue != nil, (isSubmitting || isPreparingPayload) {
            statusLabel.textColor = .secondaryLabel
        } else if validationIssue != nil {
            statusLabel.textColor = .systemRed
        } else if fallbackErrorMessage != nil {
            statusLabel.textColor = .systemRed
        } else {
            statusLabel.textColor = .secondaryLabel
        }

        saveButton.isEnabled = isReady && !isSubmitting
        cancelButton.isEnabled = !isSubmitting

        if isSubmitting || isPreparingPayload {
            activityIndicator.startAnimating()
        } else {
            activityIndicator.stopAnimating()
        }

        linksLabel.isHidden = candidates.isEmpty
        linksTableView.isHidden = candidates.isEmpty
        if candidates.isEmpty {
            linksTableHeightConstraint?.constant = 0
        } else {
            let rowsHeight = CGFloat(candidates.count) * 56.0
            linksTableHeightConstraint?.constant = min(max(rowsHeight, 56.0), 260.0)
        }

        updateTitlePlaceholderVisibility()
    }

    private func updateTitleHeight() {
        guard maxTitleHeight > 0, let heightConstraint = titleHeightConstraint else {
            return
        }

        let fittingWidth = max(titleTextView.bounds.width, view.bounds.width - 32)
        let measured = titleTextView.sizeThatFits(
            CGSize(width: fittingWidth, height: .greatestFiniteMagnitude)
        ).height
        let boundedHeight = min(max(measured, minTitleHeight), maxTitleHeight)
        if abs(heightConstraint.constant - boundedHeight) > 0.5 {
            heightConstraint.constant = boundedHeight
        }
        titleTextView.isScrollEnabled = measured > maxTitleHeight
    }

    @objc
    private func cancelTapped() {
        let error = NSError(domain: NSCocoaErrorDomain, code: NSUserCancelledError)
        extensionContext?.cancelRequest(withError: error)
    }

    @objc
    private func saveTapped() {
        guard !isSubmitting else {
            return
        }
        fallbackErrorMessage = nil
        updateUIState()
        Task {
            await submit()
        }
    }

    private func submit() async {
        guard let selectedCandidate = selectedCandidate else {
            updateUIState()
            return
        }

        isSubmitting = true
        await MainActor.run {
            updateUIState()
        }

        let resolvedTitle = titleTextView.text
            .trimmingCharacters(in: .whitespacesAndNewlines)
            .nilIfEmpty ?? extractedTitle.trimmingCharacters(in: .whitespacesAndNewlines)

        let saveResult = await executeSave(
            candidate: selectedCandidate,
            resolvedTitle: resolvedTitle
        )
        switch saveResult {
        case .delivered:
            await finishWithCompletionMessage("Saved to Hyperlinked.")
        case .queuedOffline:
            await finishWithCompletionMessage("Saved offline. Syncing later.")
        case .failed(let message):
            showBlockingError(message)
        }
    }

    private func drainPendingOutbox(
        store: ShareOutboxStore,
        client: ShareAPIClient,
        excludingID: String
    ) async {
        guard let dueItems = try? store.dueItems(limit: 8), !dueItems.isEmpty else {
            return
        }

        for item in dueItems where item.id != excludingID {
            do {
                try await client.createHyperlink(title: item.title, url: item.url)
                try store.markDelivered(id: item.id)
            } catch {
                try? store.markAttemptFailed(id: item.id, errorMessage: error.localizedDescription)
            }
        }
    }

    @MainActor
    private func showBlockingError(_ message: String) {
        isSubmitting = false
        updateUIState()
        let alert = UIAlertController(
            title: "Couldn't Save Link",
            message: message,
            preferredStyle: .alert
        )
        alert.addAction(UIAlertAction(title: "OK", style: .default))
        present(alert, animated: true)
    }

    private func finishWithCompletionMessage(_ message: String) async {
        await MainActor.run {
            statusLabel.text = message
            statusLabel.isHidden = false
            statusLabel.textColor = .secondaryLabel
        }
        try? await Task.sleep(nanoseconds: 650_000_000)
        await MainActor.run {
            extensionContext?.completeRequest(returningItems: [], completionHandler: nil)
        }
    }

    private func updateTitlePlaceholderVisibility() {
        titlePlaceholderLabel.isHidden = !titleTextView.text
            .trimmingCharacters(in: .whitespacesAndNewlines)
            .isEmpty
    }

    private func configuredServerURL() -> URL? {
        let defaults = UserDefaults(suiteName: appGroupID)
        guard let raw = defaults?.string(forKey: selectedServerURLKey) else {
            return nil
        }
        return normalizedServerURL(from: raw)
    }

    private func normalizedServerURL(from rawValue: String) -> URL? {
        let trimmed = rawValue.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else {
            return nil
        }

        let candidate = trimmed.contains("://") ? trimmed : "http://\(trimmed)"
        guard var components = URLComponents(string: candidate),
              let scheme = components.scheme?.lowercased(),
              (scheme == "http" || scheme == "https"),
              let host = components.host,
              !host.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else {
            return nil
        }

        components.user = nil
        components.password = nil
        components.path = ""
        components.query = nil
        components.fragment = nil

        guard let url = components.url else {
            return nil
        }
        let absolute = url.absoluteString
        if absolute.hasSuffix("/") {
            return URL(string: String(absolute.dropLast()))
        }
        return url
    }
}

extension ShareViewController: UITableViewDataSource, UITableViewDelegate {
    func tableView(_ tableView: UITableView, numberOfRowsInSection section: Int) -> Int {
        candidates.count
    }

    func tableView(_ tableView: UITableView, cellForRowAt indexPath: IndexPath) -> UITableViewCell {
        let identifier = "linkCandidateCell"
        let cell = tableView.dequeueReusableCell(withIdentifier: identifier)
            ?? UITableViewCell(style: .subtitle, reuseIdentifier: identifier)
        let candidate = candidates[indexPath.row]
        cell.backgroundColor = .secondarySystemBackground
        cell.textLabel?.text = candidate.displayValue
        cell.textLabel?.font = .preferredFont(forTextStyle: .body)
        cell.textLabel?.textColor = .label
        cell.textLabel?.lineBreakMode = .byTruncatingMiddle
        cell.detailTextLabel?.text = candidate.url.absoluteString
        cell.detailTextLabel?.font = .preferredFont(forTextStyle: .caption1)
        cell.detailTextLabel?.textColor = .secondaryLabel
        cell.detailTextLabel?.lineBreakMode = .byTruncatingMiddle
        cell.accessoryType = candidate.id == selectedCandidateID ? .checkmark : .none
        cell.selectionStyle = .default
        return cell
    }

    func tableView(_ tableView: UITableView, didSelectRowAt indexPath: IndexPath) {
        selectedCandidateID = candidates[indexPath.row].id
        tableView.reloadData()
        updateUIState()
    }
}

extension ShareViewController: UITextViewDelegate {
    func textViewDidChange(_ textView: UITextView) {
        updateTitlePlaceholderVisibility()
        updateTitleHeight()
    }
}

private extension String {
    var nilIfEmpty: String? {
        isEmpty ? nil : self
    }
}
